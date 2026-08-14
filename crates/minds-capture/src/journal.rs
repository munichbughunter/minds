//! Das lokale Journal: eine Datei pro Hook-Event, 0600, außerhalb von Git.
//!
//! Ein Agent-Hook ist ein Prozessstart mitten in der Arbeitsschleife des
//! Nutzers. Er darf drei Dinge tun — stdin lesen, eine Datei schreiben, mit 0
//! enden — und sonst nichts. Kein Git, keine Redaction, kein Parsen des
//! Transkripts. Alles Teure passiert später, beim Checkpoint. Dieses Modul ist
//! der „eine Datei schreiben"-Teil.
//!
//! # Warum das Journal kein Git-Ref ist
//!
//! Das Journal enthält **Rohdaten**: ungefilterte Tool-Ausgaben, Prompts,
//! möglicherweise Zugangsdaten. Genau hier hat der Stand der Technik seine
//! dokumentierte Schwachstelle — temporäre Shadow-Branches, die unredigierte
//! Daten enthalten können und „nicht gepusht werden sollten".
//!
//! „Sollte nicht" ist keine Garantie. Deshalb liegt das Journal unter
//! `<git-dir>/minds/journal/`: außerhalb des Worktrees, kein Ref, kein Objekt,
//! nicht im Index. Es kann nicht versehentlich committet und nicht
//! versehentlich gepusht werden, weil Git es schlicht nicht kennt. Rechte sind
//! 0700/0600. Nach erfolgreicher Übernahme in den Store wird es gelöscht.
//!
//! Das ist kein besserer Vorsatz, sondern eine andere Bauform: Die Zusage ist
//! strukturell, nicht disziplinarisch — dieselbe Linie wie beim
//! `RedactionAudit`, der keine Zeichenkette aus dem Eingabetext enthält.
//!
//! # Layout
//!
//! ```text
//! <git-dir>/minds/journal/
//! └─ claude-code/                     # Agentname
//!    └─ 31f3f224-f440-41ac-.../       # local_id der Agent-Session
//!       ├─ 0000000000.json            # seq 0
//!       ├─ 0000000001.json
//!       └─ .next                      # Hinweis, unverbindlich
//! ```
//!
//! # Die Sequenznummer, und warum sie ohne Sperre auskommt
//!
//! Mehrere Agents parallel heißt mehrere gleichzeitige Hook-Prozesse. Ein
//! gemeinsames Append-Log bräuchte eine Sperre auf dem heißen Pfad; eine Sperre
//! bedeutet Wartezeit im Agenten des Nutzers und im Absturzfall eine
//! verwaiste Sperrdatei.
//!
//! Stattdessen ist die **Dateierstellung selbst die Sperre**:
//! [`File::create_new`] ist atomar. Wer `0000000042.json` anlegen kann, besitzt
//! Sequenznummer 42; wer scheitert, zählt hoch und versucht es erneut. Zwei
//! Prozesse können sich nie dieselbe Nummer teilen, ohne dass jemals ein Lock
//! gehalten wird. Die Datei `.next` ist nur ein Startwert für die Suche und
//! darf beliebig falsch sein — sie spart den `read_dir`, sie garantiert nichts.
//!
//! Die Nummer ist dabei kein Ordnungs-Luxus, sondern der einzige Weg, **Lücken**
//! zu erkennen: Der Hook ist fail-open (er darf die Sitzung des Nutzers nie
//! abbrechen), also *kann* ein Event fehlen. Ein Sprung in der Folge macht das
//! sichtbar. Ehrlich lückenhaft schlägt still vollständig.
//!
//! # Schreiben in zwei Schritten
//!
//! `create_new` liefert eine **leere** Datei — ein Leser, der genau in diesem
//! Moment schaut, sähe kein halbes JSON, sondern null Bytes. Der Inhalt geht
//! deshalb nach `<seq>.json.tmp`, wird gefsynct und dann über die Reservierung
//! **umbenannt**; `rename` ist atomar. Übrig gebliebene leere Dateien oder
//! `.tmp`-Reste sind damit eindeutig als abgestürzter Schreibvorgang lesbar und
//! nicht als gültiges Event — [`Journal::read`] meldet sie als
//! [`Damaged`](ReadOutcome::Damaged), statt sie zu verschweigen.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::error::{CaptureError, Result};

/// Verzeichnis unterhalb des Git-Verzeichnisses.
const JOURNAL_DIR: &str = "minds/journal";

/// Name der unverbindlichen Startwert-Datei.
const HINT_FILE: &str = ".next";

/// Obergrenze für die Suche nach einer freien Sequenznummer. Wird sie erreicht,
/// ist etwas grundsätzlich kaputt (volles Dateisystem, falsche Rechte) und ein
/// weiterer Versuch macht es nicht besser.
const MAX_SEQ_PROBES: u64 = 4_096;

// ---------------------------------------------------------------------------
// Schlüssel
// ---------------------------------------------------------------------------

/// Welcher Agent, welche Session — der Namensraum des Journals.
///
/// Bewusst nicht global eindeutig: Zwei Agents dürfen dieselbe UUID vergeben,
/// weil der Agentname immer danebensteht. Das ist dieselbe Konvention wie bei
/// [`Lineage`](minds_core::Lineage).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    agent: String,
    local_id: String,
}

impl SessionKey {
    /// Prüft beide Bestandteile und baut den Schlüssel.
    ///
    /// # Warum das eine Sicherheitsgrenze ist
    ///
    /// `local_id` kommt aus dem JSON, das der Agent auf stdin schickt — also
    /// aus einer Quelle, die wir nicht kontrollieren. Ungeprüft in einen Pfad
    /// eingesetzt, wäre `../../hooks/pre-commit` ein Schreibzugriff auf das
    /// Git-Verzeichnis. Erlaubt sind deshalb nur `[A-Za-z0-9._-]`, nicht leer,
    /// nicht `.` oder `..`, höchstens 128 Zeichen. UUIDs und Agentnamen passen
    /// da bequem hinein; alles andere wird abgelehnt statt zurechtgebogen.
    pub fn new(agent: impl Into<String>, local_id: impl Into<String>) -> Result<Self> {
        let agent = agent.into();
        let local_id = local_id.into();
        check_component(&agent, "agent")?;
        check_component(&local_id, "local_id")?;
        Ok(Self { agent, local_id })
    }

    pub fn agent(&self) -> &str {
        &self.agent
    }

    pub fn local_id(&self) -> &str {
        &self.local_id
    }
}

fn check_component(value: &str, field: &'static str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));

    if ok {
        Ok(())
    } else {
        Err(CaptureError::UnsafeKey {
            field,
            value: value.chars().take(64).collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Ein Hook-Event, wie es im Journal liegt.
///
/// Das Format ist **unseres**, nicht das des Agenten: Ein normalisierter
/// Umschlag plus der unveränderte Original-Payload. Der Umschlag macht das
/// Journal agent-unabhängig lesbar, der Payload bleibt Beweismittel — was wir
/// heute nicht zu deuten wissen, ist morgen noch da.
///
/// Dieses Format wird **nie gehasht** und ist nicht Teil der
/// Content-Adressierung. Die Beschränkungen der kanonischen Form (nur
/// Ganzzahlen unter 2^53) gelten hier deshalb nicht.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    /// Laufende Nummer innerhalb dieser Session, lückenlos ab 0 — siehe
    /// Modul-Doku.
    pub seq: u64,

    /// Zeitpunkt der Aufzeichnung, RFC 3339 in UTC. Von *uns* gesetzt, nicht
    /// vom Agenten: Der Hook ist dabei, wenn es passiert, und ist damit die
    /// bessere Quelle als ein Feld, das nicht jeder Agent mitschickt.
    pub at: String,

    /// Dieselbe Ablesung in Nanosekunden seit Epoch — der Sortierschlüssel,
    /// wenn Events *verschiedener* Agents zusammengeführt werden.
    pub at_nanos: u64,

    /// Auf unser Vokabular normalisierte Art des Events.
    pub kind: EventKind,

    /// Der Originalname des Events beim Agenten (`PostToolUse`, …). Bleibt
    /// erhalten, auch wenn [`EventKind`] ihn auf [`EventKind::Other`] abbildet.
    pub raw_kind: String,

    /// Arbeitsverzeichnis laut Hook-Payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Pfad zum Transkript des Agenten, falls mitgeliefert.
    ///
    /// Das ist die Klammer zwischen beiden Welten: Der Hook liefert Zeitpunkt,
    /// Reihenfolge und Kausalität, das Transkript den reichen Inhalt (Volltext,
    /// Thinking, Token-Zähler), der im Hook-Payload nicht steht. Beide Hälften
    /// werden erst beim Checkpoint zusammengeführt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,

    /// Der unveränderte Payload des Agenten.
    ///
    /// [`RawValue`] statt [`serde_json::Value`], damit die Bytes exakt so
    /// erhalten bleiben, wie sie ankamen — kein Umsortieren von Schlüsseln,
    /// keine Zahlen, die durch einen Parse/Serialize-Zyklus ihre Schreibweise
    /// ändern. Beweismittel werden nicht umformatiert.
    pub payload: Box<RawValue>,
}

/// Ein Event, dem noch die Sequenznummer fehlt — die vergibt das Journal.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub at: String,
    pub at_nanos: u64,
    pub kind: EventKind,
    pub raw_kind: String,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    pub payload: Box<RawValue>,
}

/// Unser agent-unabhängiges Event-Vokabular.
///
/// Klein gehalten: Die Lebenszyklen der gängigen Agents lassen sich auf
/// „Session beginnt / Prompt / Tool davor / Tool danach / Zug endet / Sub-Agent
/// beginnt / Sub-Agent endet / Session endet" abbilden. Alles Übrige landet in
/// [`EventKind::Other`] und bleibt über `raw_kind` und `payload` vollständig
/// erhalten — ein unbekanntes Event darf nie ein Adapter-Fehler sein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStart,
    Prompt,
    ToolPre,
    ToolPost,
    TurnEnd,
    SubagentStart,
    SubagentEnd,
    SessionEnd,
    Other,
}

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

/// Das Journal eines Repositories.
#[derive(Debug, Clone)]
pub struct Journal {
    root: PathBuf,
}

impl Journal {
    /// Öffnet das Journal unterhalb eines bekannten Git-Verzeichnisses.
    ///
    /// Legt nichts an — das passiert erst beim ersten [`append`](Self::append).
    /// Ein Hook, der nichts zu schreiben hat, hinterlässt keine Spur.
    pub fn open(git_dir: &Path) -> Self {
        Self {
            root: git_dir.join(JOURNAL_DIR),
        }
    }

    /// Sucht von `start` aufwärts ein `.git` und öffnet das Journal darin.
    ///
    /// Bewusst eine eigene, dumme Suche statt `minds-git`: Der Hook soll auf
    /// dem heißen Pfad kein Repository öffnen, keine Refs auflösen und keine
    /// Konfiguration lesen. Er braucht nur ein Verzeichnis.
    ///
    /// `.git` als **Datei** (verlinkte Worktrees, Submodule) wird gelesen und
    /// dem `gitdir:`-Verweis gefolgt. Ohne das hätte jeder Worktree stillschweigend
    /// kein Journal.
    pub fn discover(start: &Path) -> Result<Self> {
        let start = start
            .canonicalize()
            .map_err(|e| CaptureError::io("Arbeitsverzeichnis auflösen", start, e))?;

        for dir in start.ancestors() {
            let candidate = dir.join(".git");
            match fs::metadata(&candidate) {
                Ok(m) if m.is_dir() => return Ok(Self::open(&candidate)),
                Ok(m) if m.is_file() => {
                    let text = fs::read_to_string(&candidate)
                        .map_err(|e| CaptureError::io("gitdir-Datei lesen", &candidate, e))?;
                    let target = text
                        .lines()
                        .find_map(|l| l.strip_prefix("gitdir:"))
                        .ok_or_else(|| CaptureError::NoRepository {
                            start: start.clone(),
                        })?
                        .trim();
                    let target = dir.join(target);
                    return Ok(Self::open(&target));
                }
                _ => continue,
            }
        }

        Err(CaptureError::NoRepository { start })
    }

    /// Das Wurzelverzeichnis des Journals.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hängt ein Event an und gibt es mit vergebener Sequenznummer zurück.
    ///
    /// Das ist die einzige Operation auf dem heißen Pfad. Sie macht: bis zu
    /// zwei `create`-Aufrufe, ein `write`, ein `fsync`, ein `rename`. Kein
    /// Netz, kein Git, keine Sperre.
    pub fn append(&self, key: &SessionKey, event: NewEvent) -> Result<JournalEvent> {
        let dir = self.session_dir(key);
        create_dir_private(&self.root, &dir)?;

        let (seq, claim) = self.reserve(&dir)?;

        let event = JournalEvent {
            seq,
            at: event.at,
            at_nanos: event.at_nanos,
            kind: event.kind,
            raw_kind: event.raw_kind,
            cwd: event.cwd,
            transcript_path: event.transcript_path,
            payload: event.payload,
        };

        let tmp = dir.join(format!("{seq:010}.json.tmp"));
        write_private(&tmp, &serde_json::to_vec(&event)?)?;
        fs::rename(&tmp, &claim).map_err(|e| CaptureError::io("Event umbenennen", &claim, e))?;

        // Auch das Verzeichnis synchronisieren (#49): `rename` ist ein Eintrag
        // im Verzeichnis, und ohne dessen fsync kann ein Stromausfall das
        // Event verschwinden lassen, obwohl der Hook Erfolg gemeldet hat — die
        // Datei selbst war schon synchronisiert, ihr Name noch nicht.
        // Kostenabwägung: ein `open` + `fsync` je Event; auf Linux/SSD wenige
        // Millisekunden, auf macOS (F_FULLFSYNC) je nach Hardware auch
        // spürbar mehr — der Hook-Pfad trägt das, und wer je gemessen
        // darunter leidet, findet hier die eine Stelle zum Abwägen. Fehler
        // nicht verschlucken — mit einer Ausnahme für Dateisysteme, die
        // Verzeichnis-fsync schlicht nicht anbieten (siehe `sync_dir`). Ein
        // Fehler hier heißt übrigens nicht „nichts persistiert": Die
        // Event-Datei liegt bereits; nur ihre Haltbarkeit über einen Crash
        // ist unbestätigt.
        sync_dir(&dir)?;

        // Unverbindlicher Startwert fuer den naechsten Aufruf. Schlaegt das
        // fehl, kostet es genau einen `read_dir` — kein Grund, das Event
        // zurueckzurollen. 0600 wie alles hier, aber ohne fsync: Der Hinweis
        // ist rekonstruierbar und darf einen Absturz nicht überleben müssen.
        let _ = write_hint(&dir.join(HINT_FILE), seq + 1);

        Ok(event)
    }

    /// Reserviert die nächste freie Sequenznummer, indem sie ihre Datei anlegt.
    fn reserve(&self, dir: &Path) -> Result<(u64, PathBuf)> {
        let mut seq = read_hint(dir).unwrap_or_else(|| scan_next_seq(dir));

        for _ in 0..MAX_SEQ_PROBES {
            let path = dir.join(format!("{seq:010}.json"));
            match File::create_new(&path) {
                Ok(_) => return Ok((seq, path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    seq += 1;
                }
                Err(e) => return Err(CaptureError::io("Event anlegen", &path, e)),
            }
        }

        Err(CaptureError::SeqExhausted {
            dir: dir.to_path_buf(),
            probes: MAX_SEQ_PROBES,
        })
    }

    /// Alle Sessions, für die Events vorliegen.
    pub fn sessions(&self) -> Result<Vec<SessionKey>> {
        let mut out = Vec::new();
        let agents = match fs::read_dir(&self.root) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(CaptureError::io("Journal lesen", &self.root, e)),
        };

        for agent in agents.flatten() {
            if !agent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let agent_name = agent.file_name().to_string_lossy().into_owned();
            let Ok(sessions) = fs::read_dir(agent.path()) else {
                continue;
            };
            for session in sessions.flatten() {
                if !session.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let local_id = session.file_name().to_string_lossy().into_owned();
                if let Ok(key) = SessionKey::new(agent_name.clone(), local_id) {
                    out.push(key);
                }
            }
        }

        out.sort();
        Ok(out)
    }

    /// Liest alle Events einer Session, nach Sequenznummer sortiert.
    ///
    /// Meldet Lücken und beschädigte Dateien mit, statt sie zu verschweigen —
    /// siehe [`ReadOutcome`].
    pub fn read(&self, key: &SessionKey) -> Result<ReadOutcome> {
        let dir = self.session_dir(key);
        let mut events = Vec::new();
        let mut damaged = Vec::new();

        let entries = match fs::read_dir(&dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadOutcome::default());
            }
            Err(e) => return Err(CaptureError::io("Session lesen", &dir, e)),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.ends_with(".json") {
                // `.tmp`-Reste und `.next` sind kein Event.
                if name.ends_with(".json.tmp") {
                    damaged.push(path);
                }
                continue;
            }

            match fs::read(&path) {
                Ok(bytes) if bytes.is_empty() => damaged.push(path),
                Ok(bytes) => match serde_json::from_slice::<JournalEvent>(&bytes) {
                    Ok(ev) => events.push(ev),
                    Err(_) => damaged.push(path),
                },
                Err(_) => damaged.push(path),
            }
        }

        events.sort_by_key(|e| e.seq);
        let gaps = gaps(&events);

        Ok(ReadOutcome {
            events,
            gaps,
            damaged,
        })
    }

    /// Löscht alle Events einer Session — der Schritt *nach* erfolgreicher
    /// Übernahme in den Store.
    ///
    /// Bis dahin bleibt das Journal liegen. Ein Absturz zwischen Capture und
    /// Store darf keine Rohdaten verlieren; er darf sie nur nicht behalten,
    /// wenn sie sicher angekommen sind.
    pub fn discard(&self, key: &SessionKey) -> Result<()> {
        let dir = self.session_dir(key);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CaptureError::io("Session verwerfen", &dir, e)),
        }
    }

    fn session_dir(&self, key: &SessionKey) -> PathBuf {
        self.root.join(&key.agent).join(&key.local_id)
    }
}

/// Ergebnis von [`Journal::read`] — inklusive dem, was fehlt.
#[derive(Debug, Default)]
pub struct ReadOutcome {
    /// Die gelesenen Events, nach `seq` sortiert.
    pub events: Vec<JournalEvent>,

    /// Sequenznummern, die zwischen der kleinsten und der größten vorhandenen
    /// fehlen. Nicht leer heißt: Der Hook ist mindestens einmal fail-open
    /// gelaufen. `minds fsck` macht das sichtbar.
    pub gaps: Vec<u64>,

    /// Dateien, die kein gültiges Event enthielten (leere Reservierung,
    /// `.tmp`-Rest, kaputtes JSON) — ein abgestürzter Schreibvorgang.
    pub damaged: Vec<PathBuf>,
}

impl ReadOutcome {
    /// `true`, wenn nichts fehlt und nichts beschädigt ist.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty() && self.damaged.is_empty()
    }
}

/// Fehlende Sequenznummern zwischen der ersten und der letzten vorhandenen.
///
/// Bewusst **nicht** ab 0 gezählt: Wenn `minds capture` einen Teil des Journals
/// bereits übernommen und gelöscht hat, beginnt der Rest legitim bei einer
/// höheren Nummer. Eine Lücke ist etwas Fehlendes *zwischen* Vorhandenem.
fn gaps(sorted: &[JournalEvent]) -> Vec<u64> {
    let (Some(first), Some(last)) = (sorted.first(), sorted.last()) else {
        return Vec::new();
    };
    let mut missing = Vec::new();
    let mut expect = first.seq;
    for ev in sorted {
        while expect < ev.seq {
            missing.push(expect);
            expect += 1;
        }
        expect = ev.seq + 1;
    }
    debug_assert!(expect == last.seq + 1);
    missing
}

// ---------------------------------------------------------------------------
// Dateisystem-Kleinkram
// ---------------------------------------------------------------------------

fn read_hint(dir: &Path) -> Option<u64> {
    fs::read_to_string(dir.join(HINT_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn scan_next_seq(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            name.strip_suffix(".json")?.parse::<u64>().ok()
        })
        .max()
        .map(|m| m + 1)
        .unwrap_or(0)
}

/// Legt das Session-Verzeichnis an — jede Journal-Ebene mit 0700, und nie
/// durch einen Symlink.
///
/// Zwei Regeln aus den Reviews zu #49:
///
/// - **Gehärtet wird ab `journal/`, nicht ab `minds/`:** Im selben
///   `<git-dir>/minds` liegen auch `hook.log`, `sync.lock` und das
///   Ref-Schreib-Lock. Wer diese Ebene selbst auf 0700 zöge, entzöge in
///   einem gruppen-geteilten Repo dem zweiten Nutzer Lock **und**
///   Fehlerkanal (#113) — sein Verlust wäre vollständig still. Die
///   Metadaten-Zusage von #49 braucht das nicht: 0700 auf `journal/` sperrt
///   alles darunter.
/// - **Kein Anlegen und kein chmod durch einen Symlink** — dieselbe
///   Invariante, die `hooklog` für dasselbe Verzeichnis verteidigt. Eine
///   verlinkte Ebene ist ein Fehler, kein Ziel.
///
/// Neue Ebenen entstehen direkt mit 0700 (`DirBuilder::mode`, kein
/// Umask-Fenster); die anschließende chmod-Wanderung heilt Bestandsjournale
/// von vor dieser Härtung. Scheitert sie, ist das ein Fehler, kein
/// Achselzucken: Rohdaten unter einer 0755-Ebene weiterzuschreiben wäre die
/// stillere und schlechtere Wahl.
fn create_dir_private(root: &Path, leaf: &Path) -> Result<()> {
    refuse_symlinked_levels(root, leaf)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(leaf)
            .map_err(|e| CaptureError::io("Journal-Verzeichnis anlegen", leaf, e))?;

        let mut level = leaf;
        loop {
            fs::set_permissions(level, fs::Permissions::from_mode(0o700))
                .map_err(|e| CaptureError::io("Journal-Verzeichnis härten", level, e))?;
            if level == root {
                break;
            }
            match level.parent() {
                Some(parent) if parent.starts_with(root) => level = parent,
                _ => break,
            }
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(leaf)
            .map_err(|e| CaptureError::io("Journal-Verzeichnis anlegen", leaf, e))?;
        let _ = root;
    }
    Ok(())
}

/// Verweigert jede Journal-Ebene, die ein Symlink ist — geprüft **vor** dem
/// Anlegen (Vorlauf-Regel), von `<git-dir>/minds` bis zum Session-Blatt.
///
/// `create_dir_all`/`set_permissions` folgen Symlinks: Ein Angreifer mit
/// Schreibrecht im Git-Verzeichnis könnte sonst Rohdaten in ein fremdes
/// Verzeichnis (oder in den committbaren Worktree) umlenken und die
/// 0700-Härtung auf ein fremdes Ziel anwenden. Zwischen Prüfung und Anlegen
/// bleibt ein schmales Fenster — dieselbe Abwägung wie im `hooklog`, dessen
/// Deskriptor-Prüfung hier für Verzeichnisse kein Gegenstück hat.
fn refuse_symlinked_levels(root: &Path, leaf: &Path) -> Result<()> {
    let base = root.parent().unwrap_or(root);
    let mut level = leaf;
    loop {
        if let Ok(meta) = fs::symlink_metadata(level) {
            if meta.file_type().is_symlink() {
                return Err(CaptureError::io(
                    "Journal-Ebene ist ein Symlink — dorthin wird nicht geschrieben",
                    level,
                    std::io::Error::other("Symlink statt Verzeichnis"),
                ));
            }
        }
        if level == base {
            return Ok(());
        }
        match level.parent() {
            Some(parent) if parent.starts_with(base) => level = parent,
            _ => return Ok(()),
        }
    }
}

/// Synchronisiert die Verzeichnis-Einträge — macht ein `rename` haltbar.
///
/// Dateisysteme ohne Verzeichnis-fsync (NFS-, SMB-, FUSE-Mounts) melden
/// `ENOTSUP`/`EINVAL` — das ist dort kein Fehler, sondern die Obergrenze
/// dessen, was das Dateisystem hergibt: Die Event-Datei selbst bleibt hart
/// synchronisiert (`write_private`), und ein harter Fehler hier machte das
/// Journal auf solchen Repos komplett funktionslos.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    match fs::File::open(dir).and_then(|f| f.sync_all()) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(CaptureError::io(
            "Journal-Verzeichnis synchronisieren",
            dir,
            e,
        )),
    }
}

/// Unter Windows gibt es keinen Verzeichnis-fsync (`File::open` auf ein
/// Verzeichnis scheitert dort grundsätzlich); das NTFS-Metadaten-Journal
/// übernimmt die Rolle.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Schreibt den `.next`-Hinweis mit 0600 — wie alles im Journal.
fn write_hint(path: &Path, next: u64) -> std::io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode()` wirkt nur bei Neuanlage — ein `.next` aus einem
        // Bestandsjournal wird hier mitgeheilt.
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let mut f = f;
    f.write_all(next.to_string().as_bytes())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| CaptureError::io("Event schreiben", path, e))?;
    f.write_all(bytes)
        .map_err(|e| CaptureError::io("Event schreiben", path, e))?;
    // Vor dem `rename` synchronisieren: Sonst kann ein Absturz eine sichtbare,
    // aber leere Datei hinterlassen — also ein Event, das es nie gab.
    f.sync_all()
        .map_err(|e| CaptureError::io("Event synchronisieren", path, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(s: &str) -> Box<RawValue> {
        RawValue::from_string(s.to_string()).unwrap()
    }

    fn event(kind: EventKind) -> NewEvent {
        let (at, at_nanos) = crate::clock::now();
        NewEvent {
            at,
            at_nanos,
            kind,
            raw_kind: format!("{kind:?}"),
            cwd: Some("/tmp/repo".into()),
            transcript_path: Some("/home/anna/.claude/projects/x/y.jsonl".into()),
            payload: raw(r#"{"tool_name":"Read"}"#),
        }
    }

    fn journal() -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(dir.path());
        (dir, j)
    }

    #[test]
    fn append_assigns_dense_sequence_numbers() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "31f3f224").unwrap();

        for expected in 0..5u64 {
            let ev = j.append(&key, event(EventKind::ToolPre)).unwrap();
            assert_eq!(ev.seq, expected);
        }

        let out = j.read(&key).unwrap();
        assert_eq!(out.events.len(), 5);
        assert!(out.is_complete());
        assert_eq!(
            out.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn a_stale_hint_costs_nothing_but_a_scan() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("codex", "abc").unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();

        // Der Hinweis darf beliebig falsch sein — die Reservierung korrigiert.
        let dir = j.session_dir(&key);
        fs::write(dir.join(HINT_FILE), "0").unwrap();

        let ev = j.append(&key, event(EventKind::Prompt)).unwrap();
        assert_eq!(ev.seq, 2, "belegte Nummern duerfen nie neu vergeben werden");
    }

    #[test]
    fn payload_bytes_survive_untouched() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "keep").unwrap();
        let mut ev = event(EventKind::ToolPost);
        // Schluesselreihenfolge und Zahlformat sind hier Absicht.
        ev.payload = raw(r#"{"z":1,"a":2,"big":10000000000000000001}"#);
        j.append(&key, ev).unwrap();

        let out = j.read(&key).unwrap();
        assert_eq!(
            out.events[0].payload.get(),
            r#"{"z":1,"a":2,"big":10000000000000000001}"#
        );
    }

    #[test]
    fn gaps_are_reported_not_hidden() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "lossy").unwrap();
        for _ in 0..4 {
            j.append(&key, event(EventKind::ToolPre)).unwrap();
        }
        // Der Hook ist fail-open: ein Event kann fehlen.
        fs::remove_file(j.session_dir(&key).join("0000000002.json")).unwrap();

        let out = j.read(&key).unwrap();
        assert_eq!(out.gaps, vec![2]);
        assert!(!out.is_complete());
    }

    #[test]
    fn gaps_do_not_count_an_already_drained_prefix() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "drained").unwrap();
        for _ in 0..3 {
            j.append(&key, event(EventKind::ToolPre)).unwrap();
        }
        for n in 0..2 {
            fs::remove_file(j.session_dir(&key).join(format!("{n:010}.json"))).unwrap();
        }

        let out = j.read(&key).unwrap();
        assert!(
            out.gaps.is_empty(),
            "ein uebernommenes Praefix ist keine Luecke"
        );
        assert_eq!(out.events[0].seq, 2);
    }

    #[test]
    fn an_empty_reservation_is_damage_not_an_event() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "crashed").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        // Genau das hinterlaesst ein Absturz zwischen `create_new` und `rename`.
        File::create_new(j.session_dir(&key).join("0000000001.json")).unwrap();

        let out = j.read(&key).unwrap();
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.damaged.len(), 1);
        assert!(!out.is_complete());
    }

    #[test]
    fn keys_reject_path_traversal() {
        assert!(SessionKey::new("claude-code", "../../hooks").is_err());
        assert!(SessionKey::new("../evil", "abc").is_err());
        assert!(SessionKey::new("claude-code", "").is_err());
        assert!(SessionKey::new("claude-code", "..").is_err());
        assert!(SessionKey::new("claude-code", "a/b").is_err());
        assert!(SessionKey::new("claude-code", "31f3f224-f440-41ac.9244").is_ok());
    }

    #[test]
    fn sessions_lists_every_agent() {
        let (_tmp, j) = journal();
        let a = SessionKey::new("claude-code", "one").unwrap();
        let b = SessionKey::new("codex", "two").unwrap();
        j.append(&a, event(EventKind::SessionStart)).unwrap();
        j.append(&b, event(EventKind::SessionStart)).unwrap();

        let found = j.sessions().unwrap();
        assert_eq!(found, vec![a, b]);
    }

    #[test]
    fn discard_removes_everything_and_is_idempotent() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "gone").unwrap();
        j.append(&key, event(EventKind::SessionEnd)).unwrap();

        j.discard(&key).unwrap();
        assert!(j.read(&key).unwrap().events.is_empty());
        j.discard(&key).unwrap();
    }

    #[test]
    fn reading_an_unknown_session_is_empty_not_an_error() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "never").unwrap();
        assert!(j.read(&key).unwrap().events.is_empty());
        assert!(j.sessions().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "perms").unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();

        let path = j.session_dir(&key).join("0000000000.json");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "das Journal enthaelt Rohdaten");

        // Auch der Hinweis — er verraet zwar nur eine Zahl, aber 0600 gilt
        // fuer alles hier.
        let hint = fs::metadata(j.session_dir(&key).join(HINT_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(hint, 0o600, "{hint:o}");

        // Und jede Journal-Ebene (#49): Ohne die Härtung entstünden
        // Zwischenebenen mit Umask-Rechten, und andere lokale Nutzer saehen
        // Agentnamen und Session-Kennungen. `minds/` selbst bleibt bewusst
        // ungehärtet — dort liegen die geteilten Koordinationsdateien
        // (hook.log, Locks), siehe `create_dir_private`.
        for dir in [
            j.root().to_path_buf(),
            j.session_dir(&key).parent().unwrap().to_path_buf(),
            j.session_dir(&key),
        ] {
            let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}: {mode:o}", dir.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_from_before_the_hardening_is_healed_on_append() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "heilung").unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();

        // Ein Bestandsjournal simulieren: alle Ebenen offen, der Hinweis 0644.
        for dir in [
            j.root().to_path_buf(),
            j.session_dir(&key).parent().unwrap().to_path_buf(),
            j.session_dir(&key),
        ] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let hint = j.session_dir(&key).join(HINT_FILE);
        fs::set_permissions(&hint, fs::Permissions::from_mode(0o644)).unwrap();

        j.append(&key, event(EventKind::Prompt)).unwrap();

        for dir in [
            j.root().to_path_buf(),
            j.session_dir(&key).parent().unwrap().to_path_buf(),
            j.session_dir(&key),
        ] {
            let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "nicht geheilt: {}", dir.display());
        }
        let mode = fs::metadata(&hint).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "der Hinweis wurde nicht geheilt: {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_journal_level_is_refused_not_followed() {
        use std::os::unix::fs::PermissionsExt;
        // Dieselbe Invariante wie im hooklog: Ein Symlink im Git-Verzeichnis
        // darf weder beschrieben noch chmodded werden — sonst lenkte er
        // Rohdaten in fremdes (oder committbares) Gebiet um.
        let git_dir = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::set_permissions(target.path(), fs::Permissions::from_mode(0o755)).unwrap();

        fs::create_dir_all(git_dir.path().join("minds")).unwrap();
        std::os::unix::fs::symlink(target.path(), git_dir.path().join("minds/journal")).unwrap();

        let j = Journal::open(git_dir.path());
        let key = SessionKey::new("claude-code", "symlink").unwrap();
        let err = j.append(&key, event(EventKind::Prompt)).unwrap_err();
        assert!(err.to_string().contains("Symlink"), "{err}");

        // Das Ziel blieb unberührt: keine Dateien, Rechte unverändert.
        assert!(fs::read_dir(target.path()).unwrap().next().is_none());
        let mode = fs::metadata(target.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "{mode:o}");
    }
}
