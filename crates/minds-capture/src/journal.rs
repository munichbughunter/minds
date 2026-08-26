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
//!    └─ b3-3f2a9c1b5d7e8f04/          # blake3(local_id), gekürzt — siehe unten
//!       ├─ .key                       # der echte Schlüssel, verbindlich
//!       ├─ 0000000000.json            # seq 0
//!       ├─ 0000000001.json
//!       └─ .next                      # Hinweis, unverbindlich
//! ```
//!
//! # Verzeichnisname und Schlüssel (#95)
//!
//! Der Verzeichnisname unter einem Agenten ist **kein** lesbares `local_id`
//! mehr, sondern `b3-` plus 16 Hex-Ziffern von `blake3(local_id)`. Seit #35
//! gilt `local_id` als fremdbestimmter Wert, der alles Mögliche enthalten kann
//! — auch ein Token, dessen Alphabet exakt in das von [`SessionKey`] geprüfte
//! Zeichenrepertoire passt. Ein Verzeichnisname steht aber in jedem `ls`,
//! jedem Backup-Manifest und jedem Editor-Dateibaum; der Hash nimmt ihn aus
//! all diesen Kanälen heraus. Der Agentname bleibt roh: Er ist unser eigenes,
//! kleines Vokabular, keine fremdgegebene Kennung.
//!
//! Weil [`JournalEvent`] Agent und `local_id` nie trug (der Schlüssel lebte
//! bisher nur im Pfad), braucht der Schlüssel einen neuen Ort: `.key`, eine
//! kleine, versionierte JSON-Datei im Session-Verzeichnis, geschrieben mit
//! denselben Rechten und derselben Zwei-Schritt-Haltbarkeit wie ein Event —
//! ihr Verlust kostete nicht ein Event, sondern die Identität der ganzen
//! Session. [`Journal::sessions`] liest sie zurück, statt den Verzeichnisnamen
//! zu deuten: Der Name ist ab hier nur noch ein Bucket, kein Datum.
//!
//! `.key` ist zugleich die Verteidigung gegen den Angriff, den der Hash erst
//! möglich macht: Ein Agent könnte sein `local_id` wörtlich auf den
//! Verzeichnisnamen einer fremden Session setzen (Hex und `b3-` passieren die
//! Zeichenprüfung) und so versuchen, deren Verzeichnis zu übernehmen. Deshalb
//! wird `.key` bei **jedem** [`append`](Journal::append) gegen den erwarteten
//! Schlüssel geprüft und ein Alt-Verzeichnis nur dann migriert, wenn es
//! **kein** `.key` trägt — ein Verzeichnis mit fremdem Schlüssel wird nie
//! beschrieben, nie verschoben, nie zusammengelegt.
//!
//! Bestandsverzeichnisse von vor dieser Härtung tragen den rohen
//! `local_id`-Namen weiter. Sie werden nicht per Kommando migriert, sondern
//! beim nächsten `append` derselben Session an ihren gehashten Platz
//! verschoben — dasselbe „Heilung beim Zugriff"-Muster wie die 0700-Härtung
//! aus #49. Bis dahin bleiben sie für `sessions`, `read` und `discard`
//! vollständig sichtbar und nutzbar.
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

use minds_core::ContentHash;
use minds_core::evidence::{self, EventFacts};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::error::{CaptureError, Result};

/// Verzeichnis unterhalb des Git-Verzeichnisses.
const JOURNAL_DIR: &str = "minds/journal";

/// Name der unverbindlichen Startwert-Datei.
const HINT_FILE: &str = ".next";

/// Name der Schlüssel-Datei im Session-Verzeichnis (#95). Anders als
/// [`HINT_FILE`] ist sie verbindlich: Sie ist die einzige Quelle des rohen
/// `local_id`, sobald der Verzeichnisname nur noch dessen Hash trägt.
const KEY_FILE: &str = ".key";

/// Präfix des gehashten Verzeichnisnamens — benennt den Algorithmus, dieselbe
/// Konvention wie bei [`SessionId`](minds_core::SessionId).
const DIR_HASH_PREFIX: &str = "b3-";

/// Länge des Hex-Anteils im Verzeichnisnamen: 8 Byte / 64 Bit von blake3.
///
/// Kurz genug für ein lesbares `ls`, und als Bucket-Namensraum innerhalb
/// *eines* Agenten weit überdimensioniert (Geburtstagsgrenze ~2³² Sessions).
/// Die Länge ist trotzdem keine Sicherheitsgarantie für sich — die tragende
/// Verteidigung ist die `.key`-Prüfung in [`Journal::append`], nicht die
/// Kollisionsstatistik.
const DIR_HASH_HEX_LEN: usize = 16;

/// Aktuelle Fassung von [`KeyRecord`].
const KEY_RECORD_VERSION: u8 = 1;

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

    /// Anzeige für Menschen: `agent` roh, `local_id` durch die Pipeline (#95).
    ///
    /// Bewusst keine `Display`-Implementierung: Ein `Display`-Impl lüde dazu
    /// ein, `{key}` zu schreiben und die Redaktion zu vergessen — genau der
    /// Fehler, den diese Methode ausschließen soll. Der Pipeline-Parameter ist
    /// Pflicht, nicht optionaler Kontext; wer ihn nicht hat, kann diese
    /// Methode nicht aufrufen.
    ///
    /// `agent` bleibt roh (Scope-Entscheidung #95, siehe [`check_component`]):
    /// unser eigenes Vokabular, kein fremder Text.
    pub fn display_redacted(&self, pipeline: &minds_redact::RedactionPipeline) -> String {
        format!("{}/{}", self.agent, pipeline.redact(&self.local_id).text)
    }
}

/// Prüft die Zeichen einer Pfadkomponente — die einzige Stelle, an der
/// `agent`/`local_id` je ihr Zeichenrepertoire bestätigen.
///
/// Diese Prüfung garantiert **Pfadsicherheit**, keine Inhaltssicherheit: Das
/// erlaubte Alphabet `[A-Za-z0-9._-]` ist exakt das Alphabet gängiger Token
/// (`glpat-…`, `ghp_…`, `AKIA…`), ein Token als `local_id` besteht sie also
/// anstandslos. Seit #35 gilt der Wert deshalb als fremdbestimmt und wird im
/// Envelope mitredigiert; #95 zieht dieselbe Linie außerhalb des Envelopes:
///
/// - Der **Verzeichnisname** trägt nur noch `blake3(local_id)` — siehe
///   Modul-Doku „Verzeichnisname und Schlüssel".
/// - Jede **Anzeige** läuft durch [`SessionKey::display_redacted`], nie über
///   ein `Display`-Impl, das es bewusst nicht gibt.
///
/// Die andere Hälfte dieser Entscheidung — warum gescannt statt validiert
/// wird — steht in der Modul-Doku von `minds_redact::session`.
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
            len: value.chars().count(),
        })
    }
}

/// Der auf Platte persistierte Schlüssel (`.key`).
///
/// Vorwärts-tolerant gelesen (kein `deny_unknown_fields`) — ein alter Reader
/// darf an einem neuen Feld nicht zerbrechen. `version` ist trotzdem ein
/// exakter Vergleich beim Lesen: Ändert sich je die *Bedeutung* eines Feldes,
/// muss das eine neue Version sein, kein stillschweigend anders gelesenes
/// altes Feld.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyRecord {
    version: u8,
    agent: String,
    local_id: String,
}

/// Der Verzeichnisname zu einem `local_id`: `b3-` plus die ersten
/// [`DIR_HASH_HEX_LEN`] Hex-Ziffern von `blake3(local_id)`.
pub(crate) fn hashed_dir_name(local_id: &str) -> String {
    use std::fmt::Write as _;
    let digest = blake3::hash(local_id.as_bytes());
    let mut out = String::with_capacity(DIR_HASH_PREFIX.len() + DIR_HASH_HEX_LEN);
    out.push_str(DIR_HASH_PREFIX);
    for byte in &digest.as_bytes()[..DIR_HASH_HEX_LEN / 2] {
        let _ = write!(out, "{byte:02x}");
    }
    out
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
/// Der **Umschlag** wird nie kanonisch gehasht und ist nicht Teil der
/// Content-Adressierung — die Beschränkungen der kanonischen Form (nur
/// Ganzzahlen unter 2^53) gelten hier deshalb nicht. Seit ADR-0011 trägt
/// jedes Event stattdessen zwei **gestempelte** Hashes über eine eigene,
/// längenpräfixierte Kodierung (`minds_core::evidence`): den Hash des
/// Payloads und den Hash der beobachteten Fakten. Beide sind
/// selbstbeschreibend (kein prev-Link — die Verkettung entsteht beim Seal,
/// ADR-0011 Entscheidung 1) und machen nachträglichen Payload-Tausch an
/// liegenden Journalen erkennbar.
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

    /// `derive_key`-Hash über die Payload-Bytes, beim Append gestempelt.
    ///
    /// Über den Payload **nach** der Secretwall — für Secret-Dateien existiert
    /// damit nie ein Hash über geheimen Inhalt (Orakel-Regel). `None` nur bei
    /// Alt-Events aus der Zeit vor der Evidence-Chain (`pre_chain`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_hash: Option<ContentHash>,

    /// Hash über die beobachteten Fakten (`seq`, Zeit, `raw_kind`, `cwd`,
    /// Transkript-Pfad, `payload_hash`), beim Append gestempelt.
    ///
    /// Bewusst ohne [`kind`](Self::kind): Die Klassifikation ist
    /// Interpretation und bleibt außerhalb der Evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_hash: Option<ContentHash>,
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
    /// Netz, kein Git, keine Sperre. Seit #95 kommen dazu: ein Lesen der
    /// `.key`-Datei je Aufruf, und **einmal pro Session** (beim ersten Event)
    /// deren Schreiben mit eigenem fsync — die Identität der Session hängt an
    /// dieser Datei, sie muss einen Absturz genauso überleben wie ein Event.
    pub fn append(&self, key: &SessionKey, event: NewEvent) -> Result<JournalEvent> {
        let dir = self.session_dir(key);
        self.migrate_legacy_dir(key, &dir)?;
        create_dir_private(&self.root, &dir)?;
        self.ensure_key_file(&dir, key)?;

        let (seq, claim) = self.reserve(&dir)?;

        // Die zwei Stempel der Evidence-Chain (ADR-0011): billig (zwei
        // blake3-Läufe im Speicher), ohne zusätzliche fsyncs — sie wandern in
        // dieselbe Datei, die ohnehin geschrieben wird. Der `event_hash`
        // braucht die Sequenznummer und entsteht deshalb erst nach `reserve`.
        let payload_hash = evidence::payload_hash(event.payload.get().as_bytes());
        let event_hash = evidence::event_hash(&EventFacts {
            seq,
            at: &event.at,
            at_nanos: event.at_nanos,
            raw_kind: &event.raw_kind,
            cwd: event.cwd.as_deref(),
            transcript_path: event.transcript_path.as_deref(),
            payload_hash: &payload_hash,
        });

        let event = JournalEvent {
            seq,
            at: event.at,
            at_nanos: event.at_nanos,
            kind: event.kind,
            raw_kind: event.raw_kind,
            cwd: event.cwd,
            transcript_path: event.transcript_path,
            payload: event.payload,
            payload_hash: Some(payload_hash),
            event_hash: Some(event_hash),
        };

        let tmp = dir.join(format!("{seq:010}.json.tmp"));
        write_private(&tmp, &serde_json::to_vec(&event)?, "Event schreiben")?;
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

    /// Alle Sessions, für die Events vorliegen — inklusive dem, was sich nicht
    /// zuordnen ließ.
    ///
    /// Der Schlüssel kommt aus `.key`, nicht aus dem Verzeichnisnamen (#95).
    /// Fehlt `.key`, ist das Verzeichnis ein Bestand von vor der Härtung und
    /// sein Name selbst das `local_id` — der eine, dokumentierte Fallback.
    /// Ist `.key` dagegen vorhanden, aber unlesbar oder in sich widersprüchlich,
    /// wandert das Verzeichnis nach [`SessionsOutcome::unresolved`] statt still
    /// zu verschwinden: Dort liegen möglicherweise vollständige Events, nur
    /// ihre Identität ist verloren — ehrlich lückenhaft schlägt still
    /// vollständig.
    pub fn sessions(&self) -> Result<SessionsOutcome> {
        let mut out = SessionsOutcome::default();
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
                let path = session.path();
                let dir_name = session.file_name().to_string_lossy().into_owned();
                match read_key_record(&path) {
                    // Modernes Verzeichnis: `.key` ist die Quelle. Agent und
                    // Hash müssen zum Fundort passen — ein kopiertes oder
                    // untergeschobenes Verzeichnis fällt hier auf, statt als
                    // Phantom-Schlüssel aufzutauchen, der nirgends auflösbar
                    // wäre.
                    Some(Ok(record)) => {
                        let consistent = record.version == KEY_RECORD_VERSION
                            && record.agent == agent_name
                            && hashed_dir_name(&record.local_id) == dir_name;
                        let key = consistent
                            .then(|| SessionKey::new(record.agent, record.local_id).ok())
                            .flatten();
                        match key {
                            Some(key) => out.keys.push(key),
                            None => out.unresolved.push(displayable_unresolved(path, &dir_name)),
                        }
                    }
                    Some(Err(_)) => out.unresolved.push(displayable_unresolved(path, &dir_name)),
                    // Kein `.key`: Bestand von vor #95, der Name ist das
                    // `local_id`. Was die Zeichenprüfung nicht besteht, ist
                    // ein fremdes Verzeichnis, nie eines von uns — und wird
                    // wie eh und je still übergangen. Ohne ein einziges Event
                    // ebenso: Ein leeres Verzeichnis ist ein Absturzrest der
                    // Anlage, keine Session — als Schlüssel gedeutet stünde
                    // sein Name (auch ein Hash-Bucket, dessen `.key` nie
                    // geschrieben wurde) für immer als Phantom im fsck-Bericht.
                    None => {
                        if has_event_files(&path) {
                            if let Ok(key) = SessionKey::new(agent_name.clone(), dir_name) {
                                out.keys.push(key);
                            }
                        }
                    }
                }
            }
        }

        out.keys.sort();
        // Ein verlorener Migrations-Wettlauf kann dieselbe Session kurzzeitig
        // zweimal liefern — einmal über `.key` im gehashten Verzeichnis,
        // einmal über den noch liegenden Alt-Namen. Doppelt verarbeiten wäre
        // doppelt abgelegt.
        out.keys.dedup();
        out.unresolved.sort();
        Ok(out)
    }

    /// Liest alle Events einer Session, nach Sequenznummer sortiert.
    ///
    /// Meldet Lücken und beschädigte Dateien mit, statt sie zu verschweigen —
    /// siehe [`ReadOutcome`].
    pub fn read(&self, key: &SessionKey) -> Result<ReadOutcome> {
        let Some(dir) = self.resolve_session_dir(key) else {
            return Ok(ReadOutcome::default());
        };
        let mut events = Vec::new();
        let mut damaged = Vec::new();

        let entries = match fs::read_dir(&dir) {
            Ok(it) => it,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadOutcome::default());
            }
            // Der physische Ort kann das Alt-Verzeichnis mit rohem Namen
            // sein; der Fehlertext nennt stattdessen den gehashten Pfad
            // derselben Session — er wandert über den Checkpoint ins
            // hook.log, und dort darf kein rohes local_id stehen (#95).
            Err(e) => return Err(CaptureError::io("Session lesen", self.session_dir(key), e)),
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
        let Some(dir) = self.resolve_session_dir(key) else {
            return Ok(());
        };
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            // Wie in `read`: nie den (möglicherweise rohen) Alt-Pfad in den
            // Fehlertext — der landet im hook.log (#95).
            Err(e) => Err(CaptureError::io(
                "Session verwerfen",
                self.session_dir(key),
                e,
            )),
        }
    }

    /// Der kanonische (gehashte) Pfad — das Ziel jedes neuen Schreibvorgangs.
    fn session_dir(&self, key: &SessionKey) -> PathBuf {
        self.root
            .join(&key.agent)
            .join(hashed_dir_name(&key.local_id))
    }

    /// Der Pfad von vor #95: Das Blatt hieß `local_id` selbst. Wird nur noch
    /// gelesen und migriert, nie neu angelegt.
    fn legacy_session_dir(&self, key: &SessionKey) -> PathBuf {
        self.root.join(&key.agent).join(&key.local_id)
    }

    /// Wo die Session tatsächlich liegt: der gehashte Pfad, wenn er zu ihr
    /// gehört, sonst — für einen Bestand, der seit #95 nie wieder `append`
    /// gesehen hat — der rohe. Ohne diesen Fallback hielte
    /// [`read`](Self::read) eine volle Alt-Session für leer und
    /// [`discard`](Self::discard) löschte nach dem Checkpoint nichts, sodass
    /// derselbe Bestand bei jedem Lauf erneut verarbeitet würde.
    ///
    /// `None` heißt: Es gibt keinen Ort, der sicher zu dieser Session gehört.
    /// Dieselbe `.key`-Prüfung wie beim [`append`](Self::append), aus
    /// demselben Grund — ein Bucket, dessen Schlüssel widerspricht oder
    /// unlesbar ist, gehört jemand anderem oder niemandem Bestimmbarem, und
    /// aus ihm wird weder gelesen noch in ihm gelöscht. Ein Bucket ganz ohne
    /// `.key` dagegen ist ein Absturzrest unserer eigenen Anlage (die `.key`
    /// entsteht vor dem ersten Event) und darf behandelt werden: `read`
    /// findet dort nichts, `discard` räumt ihn auf.
    ///
    /// Ein Alt-Kandidat zählt nur ohne `.key`: Trägt das Verzeichnis unter dem
    /// rohen Namen eine Schlüssel-Datei, ist es der Hash-Bucket einer
    /// *anderen* Session, deren Name zufällig (oder absichtlich, siehe
    /// Modul-Doku) wie dieses `local_id` aussieht — nie unser Bestand.
    fn resolve_session_dir(&self, key: &SessionKey) -> Option<PathBuf> {
        let hashed = self.session_dir(key);
        match read_key_record(&hashed) {
            Some(Ok(record)) => {
                let ours = record.version == KEY_RECORD_VERSION
                    && record.agent == key.agent
                    && record.local_id == key.local_id;
                return ours.then_some(hashed);
            }
            Some(Err(_)) => return None,
            None => {
                if fs::symlink_metadata(&hashed).is_ok() {
                    return Some(hashed);
                }
            }
        }
        let legacy = self.legacy_session_dir(key);
        if matches!(fs::symlink_metadata(&legacy), Ok(m) if m.is_dir())
            && read_key_record(&legacy).is_none()
        {
            return Some(legacy);
        }
        None
    }

    /// Verschiebt ein Bestandsverzeichnis an seinen gehashten Platz — Inhalt
    /// und Sequenznummern bleiben erhalten. Kein Migrationskommando, sondern
    /// dasselbe „Heilung beim Zugriff"-Muster wie die Rechte-Härtung aus #49.
    ///
    /// No-op, wenn das Ziel schon existiert oder kein Alt-Verzeichnis
    /// vorliegt. Ein Kandidat mit `.key` ist kein Alt-Verzeichnis, sondern der
    /// Bucket einer fremden Session — der wird nicht angefasst (siehe
    /// Modul-Doku, Angriff über einen hash-förmigen `local_id`).
    fn migrate_legacy_dir(&self, key: &SessionKey, hashed: &Path) -> Result<()> {
        if fs::symlink_metadata(hashed).is_ok() {
            return Ok(());
        }
        let legacy = self.legacy_session_dir(key);
        if !matches!(fs::symlink_metadata(&legacy), Ok(m) if m.is_dir())
            || read_key_record(&legacy).is_some()
        {
            return Ok(());
        }
        // Die Ebenen oberhalb des Blatts vor dem Verschieben prüfen — das
        // Blatt selbst hat der `is_dir`-Test oben schon als Nicht-Symlink
        // bestätigt (`symlink_metadata` folgt nicht). Bewusst nicht
        // `refuse_symlinked_levels(root, legacy)`: Dessen Fehlertext nennt die
        // beanstandete Ebene, und das Blatt trüge den rohen `local_id` ins
        // hook.log (#95). Wird das Blatt *zwischen* Prüfung und `rename` zum
        // Symlink getauscht, verschiebt `rename` nur den Link; die Anlage
        // danach verweigert ihn fail-closed — dieselbe akzeptierte
        // Fensterbreite wie im hooklog.
        if let Some(agent_dir) = legacy.parent() {
            refuse_symlinked_levels(&self.root, agent_dir)?;
        }
        match fs::rename(&legacy, hashed) {
            Ok(()) => {}
            // Wettlauf verloren, das Ziel steht — ein zweiter Hook-Prozess
            // war schneller. Dann gibt es nichts mehr zu migrieren.
            Err(_) if fs::symlink_metadata(hashed).is_ok() => return Ok(()),
            // Der Fehlertext nennt das Ziel, nicht die Quelle: Der Quellname
            // ist das rohe `local_id`, und die Meldung wandert ins hook.log
            // (#95). Der gehashte Name bezeichnet dieselbe Session.
            Err(e) => return Err(CaptureError::io("Bestandssession migrieren", hashed, e)),
        }
        // Das `rename` ist ein Eintrag im Agent-Verzeichnis — haltbar machen,
        // wie beim Event-`rename` (#49).
        if let Some(agent_dir) = hashed.parent() {
            sync_dir(agent_dir)?;
        }
        Ok(())
    }

    /// Schreibt `.key` beim ersten Event einer Session und prüft sie bei jedem
    /// weiteren.
    ///
    /// Die Prüfung ist die tragende Verteidigung des gehashten Layouts: Eine
    /// Abweichung heißt Hash-Kollision, untergeschobene Kennung oder
    /// beschädigte Datei — in allen drei Fällen wird in dieses Verzeichnis
    /// **nicht** geschrieben, statt Events zweier Sessions zu vermischen.
    fn ensure_key_file(&self, dir: &Path, key: &SessionKey) -> Result<()> {
        match read_key_record(dir) {
            Some(Ok(record)) => {
                let ok = record.version == KEY_RECORD_VERSION
                    && record.agent == key.agent
                    && record.local_id == key.local_id;
                if ok {
                    Ok(())
                } else {
                    Err(CaptureError::KeyFileMismatch {
                        dir: dir.to_path_buf(),
                    })
                }
            }
            // Ein Parse-Fehler ist ein Mismatch (der Inhalt bestätigt nichts);
            // ein echter I/O-Fehler (Rechte, Medium) ist keiner — ihn als
            // „Kollision" zu melden schickte die Diagnose in die falsche
            // Richtung. Der Pfad ist der gehashte Bucket, kein Leck.
            Some(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                Err(CaptureError::KeyFileMismatch {
                    dir: dir.to_path_buf(),
                })
            }
            Some(Err(e)) => Err(CaptureError::io(
                "Schlüssel-Datei lesen",
                dir.join(KEY_FILE),
                e,
            )),
            None => {
                let record = KeyRecord {
                    version: KEY_RECORD_VERSION,
                    agent: key.agent.clone(),
                    local_id: key.local_id.clone(),
                };
                // Zwei-Schritt-Haltbarkeit wie ein Event: Ein Absturz darf
                // keine sichtbare, halbe `.key` hinterlassen — die wäre beim
                // nächsten `append` ein falscher Kollisionsalarm. Der
                // Tmp-Name trägt die Prozess-Id, denn anders als bei Events
                // reserviert hier kein `create_new` exklusiv: Zwei
                // gleichzeitige erste Events derselben Session schrieben
                // sonst in dieselbe Tmp-Datei, und der Verlierer des
                // anschließenden `rename` risse dem Gewinner die live
                // gewordene `.key` unter dem Deskriptor weg.
                let path = dir.join(KEY_FILE);
                let tmp = dir.join(format!("{KEY_FILE}.{}.tmp", std::process::id()));
                write_private(
                    &tmp,
                    &serde_json::to_vec(&record)?,
                    "Schlüssel-Datei schreiben",
                )?;
                match fs::rename(&tmp, &path) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        // Beide Prozesse schreiben denselben Inhalt — steht
                        // inzwischen die richtige `.key`, ist nichts verloren.
                        let _ = fs::remove_file(&tmp);
                        match read_key_record(dir) {
                            Some(Ok(record))
                                if record.version == KEY_RECORD_VERSION
                                    && record.agent == key.agent
                                    && record.local_id == key.local_id =>
                            {
                                Ok(())
                            }
                            _ => Err(CaptureError::io("Schlüssel-Datei umbenennen", &path, e)),
                        }
                    }
                }
            }
        }
    }
}

/// Liest die `.key`-Datei eines Session-Verzeichnisses.
///
/// `None`: keine Datei — Bestand von vor #95 oder frisch angelegt.
/// `Some(Err(_))`: vorhanden, aber unlesbar — das darf nie stillschweigend
/// wie „keine" behandelt werden, sonst würde ein beschädigter Schlüssel zum
/// Alt-Verzeichnis umgedeutet.
fn read_key_record(dir: &Path) -> Option<std::io::Result<KeyRecord>> {
    match fs::read(dir.join(KEY_FILE)) {
        Ok(bytes) => Some(
            serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e)),
    }
}

/// `true`, wenn `name` die Form eines von uns erzeugten Hash-Buckets hat.
fn looks_like_hashed_dir(name: &str) -> bool {
    name.strip_prefix(DIR_HASH_PREFIX).is_some_and(|hex| {
        hex.len() == DIR_HASH_HEX_LEN
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    })
}

/// `true`, wenn im Verzeichnis mindestens ein Event liegt.
fn has_event_files(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".json"))
            })
        })
        .unwrap_or(false)
}

/// Der Pfad eines unauflösbaren Verzeichnisses, wie er gemeldet werden darf.
///
/// [`SessionsOutcome::unresolved`] verspricht: nie ein rohes `local_id` im
/// Pfad. Ein Blattname in Hash-Form hält das von selbst; jeder andere Name
/// (ein Alt-Verzeichnis, in das eine `.key` geraten ist) könnte die rohe
/// Kennung sein und wird durch `…` ersetzt — die Zusage wird hier erzwungen,
/// nicht nur behauptet (#95). Das betroffene Verzeichnis bleibt auffindbar:
/// Es ist das eine unter dem angezeigten Agenten, das aus der Reihe fällt.
fn displayable_unresolved(path: PathBuf, dir_name: &str) -> PathBuf {
    if looks_like_hashed_dir(dir_name) {
        path
    } else {
        path.with_file_name("…")
    }
}

/// Ergebnis von [`Journal::sessions`] — inklusive dem, was sich nicht
/// zuordnen ließ (#95).
#[derive(Debug, Default)]
pub struct SessionsOutcome {
    /// Sessions mit rekonstruierbarem Schlüssel, sortiert.
    pub keys: Vec<SessionKey>,

    /// Session-Verzeichnisse, deren Schlüssel sich nicht rekonstruieren ließ:
    /// `.key` ist vorhanden, aber unlesbar oder in sich widersprüchlich. Die
    /// Events dort liegen möglicherweise vollständig vor — nur ihre Identität
    /// ist verloren. Sichtbar gemacht von `minds fsck` und `minds checkpoint`,
    /// nie stillschweigend übersprungen. Der Pfad trägt nur Agentname und
    /// Hash, nie ein rohes `local_id` — er darf angezeigt werden.
    pub unresolved: Vec<PathBuf>,
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
pub(crate) fn create_dir_private(root: &Path, leaf: &Path) -> Result<()> {
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

pub(crate) fn write_private(path: &Path, bytes: &[u8], op: &'static str) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| CaptureError::io(op, path, e))?;
    f.write_all(bytes)
        .map_err(|e| CaptureError::io(op, path, e))?;
    // Vor dem `rename` synchronisieren: Sonst kann ein Absturz eine sichtbare,
    // aber leere Datei hinterlassen — also ein Event (oder eine `.key`), das
    // es nie gab.
    f.sync_all().map_err(|e| CaptureError::io(op, path, e))?;
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
    fn every_appended_event_carries_recomputable_stamps() {
        // Die zwei Stempel aus ADR-0011: vorhanden — und aus den abgelegten
        // Feldern exakt nachrechenbar, so wie es spaeter fsck tut.
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "s1").unwrap();
        let stored = j.append(&key, event(EventKind::ToolPost)).unwrap();

        let payload = evidence::payload_hash(stored.payload.get().as_bytes());
        assert_eq!(stored.payload_hash.as_ref(), Some(&payload));
        let facts = EventFacts {
            seq: stored.seq,
            at: &stored.at,
            at_nanos: stored.at_nanos,
            raw_kind: &stored.raw_kind,
            cwd: stored.cwd.as_deref(),
            transcript_path: stored.transcript_path.as_deref(),
            payload_hash: &payload,
        };
        assert_eq!(stored.event_hash, Some(evidence::event_hash(&facts)));

        // Und auch nach dem Zurueck-Lesen von der Platte.
        let read = j.read(&key).unwrap().events;
        assert_eq!(read[0].payload_hash, stored.payload_hash);
        assert_eq!(read[0].event_hash, stored.event_hash);
    }

    #[test]
    fn a_pre_chain_event_without_stamps_still_reads() {
        // Bestand aus der Zeit vor der Evidence-Chain: kein Fehler, die
        // Stempel fehlen einfach — `pre_chain` ist ein darstellbarer Zustand.
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "alt").unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();
        let dir = j.session_dir(&key);

        // Ein Alt-Event von Hand, ohne die neuen Felder.
        let legacy = r#"{"seq":1,"at":"2026-01-01T00:00:00Z","at_nanos":1,"kind":"prompt","raw_kind":"UserPromptSubmit","payload":{"prompt":"x"}}"#;
        fs::write(dir.join("0000000001.json"), legacy).unwrap();

        let read = j.read(&key).unwrap();
        assert!(read.gaps.is_empty());
        assert!(read.events[0].payload_hash.is_some());
        assert!(read.events[1].payload_hash.is_none());
        assert!(read.events[1].event_hash.is_none());
    }

    #[test]
    fn the_payload_stamp_covers_the_walled_payload_not_the_original() {
        // Orakel-Regel: Trifft die Secretwall, wird der ersetzte Payload
        // gehasht — der Stempel verraet nichts ueber den Originalinhalt und
        // ist trotzdem nachrechenbar.
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "wall").unwrap();
        let (at, at_nanos) = crate::clock::now();
        let mut walled = NewEvent {
            at,
            at_nanos,
            kind: EventKind::ToolPre,
            raw_kind: "PreToolUse".into(),
            cwd: None,
            transcript_path: None,
            payload: raw(r#"{"tool_name":"Read","tool_input":{"file_path":"/repo/.env"}}"#),
        };
        let reason = crate::secretwall::guard(&mut walled);
        assert!(reason.is_some(), "die Wall muss greifen");
        let walled_bytes = walled.payload.get().as_bytes().to_vec();

        let stored = j.append(&key, walled).unwrap();
        assert_eq!(
            stored.payload_hash,
            Some(evidence::payload_hash(&walled_bytes))
        );
        // Der gestempelte Hash ist NICHT der Hash des Originals.
        assert_ne!(
            stored.payload_hash,
            Some(evidence::payload_hash(
                br#"{"tool_name":"Read","tool_input":{"file_path":"/repo/.env"}}"#
            ))
        );
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
        assert_eq!(found.keys, vec![a, b]);
        assert!(found.unresolved.is_empty());
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
        assert!(j.sessions().unwrap().keys.is_empty());
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

        // Und die Schluessel-Datei (#95) — sie traegt das rohe local_id.
        let key_mode = fs::metadata(j.session_dir(&key).join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(key_mode, 0o600, "{key_mode:o}");

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

    /// Macht aus einer modern angelegten Session ein Bestandsverzeichnis von
    /// vor #95: roher `local_id`-Name, keine `.key`-Datei.
    fn make_legacy(j: &Journal, key: &SessionKey) -> PathBuf {
        let hashed = j.session_dir(key);
        let legacy = hashed.parent().unwrap().join(key.local_id());
        fs::remove_file(hashed.join(KEY_FILE)).unwrap();
        fs::rename(&hashed, &legacy).unwrap();
        legacy
    }

    #[test]
    fn directory_names_are_a_short_hash_not_the_local_id() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "glpat-ABCDEFGHIJ1234567890").unwrap();
        j.append(&key, event(EventKind::Prompt)).unwrap();

        let dir = j.session_dir(&key);
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(DIR_HASH_PREFIX), "{name}");
        assert_eq!(name.len(), DIR_HASH_PREFIX.len() + DIR_HASH_HEX_LEN);

        // Nirgends unter der Journal-Wurzel trägt ein Pfadsegment die Kennung.
        fn walk(dir: &Path, needle: &str) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let name = entry.file_name();
                assert!(
                    !name.to_string_lossy().contains(needle),
                    "roher local_id im Dateisystem: {}",
                    entry.path().display()
                );
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), needle);
                }
            }
        }
        walk(j.root(), "glpat");
    }

    #[test]
    fn a_key_file_is_written_on_the_first_event_and_verified_on_the_next() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "31f3f224").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();

        let record: KeyRecord =
            serde_json::from_slice(&fs::read(j.session_dir(&key).join(KEY_FILE)).unwrap()).unwrap();
        assert_eq!(record.version, KEY_RECORD_VERSION);
        assert_eq!(record.agent, "claude-code");
        assert_eq!(record.local_id, "31f3f224");

        // Ein zweites Event derselben Session besteht die Prüfung.
        j.append(&key, event(EventKind::Prompt)).unwrap();
    }

    #[test]
    fn a_key_file_naming_a_different_session_is_refused_not_overwritten() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "mine").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();

        // Simulierte Kollision: `.key` behauptet eine andere Session.
        let foreign = KeyRecord {
            version: KEY_RECORD_VERSION,
            agent: "claude-code".into(),
            local_id: "theirs".into(),
        };
        let path = j.session_dir(&key).join(KEY_FILE);
        fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();

        let err = j.append(&key, event(EventKind::Prompt)).unwrap_err();
        assert!(matches!(err, CaptureError::KeyFileMismatch { .. }), "{err}");
        // Der Fehlertext trägt kein rohes local_id — er wandert ins hook.log.
        assert!(!err.to_string().contains("mine"), "{err}");
        assert!(!err.to_string().contains("theirs"), "{err}");
        // Auch die kalten Pfade fassen den unbestätigten Bucket nicht an:
        // `read` liefert fail-closed nichts, `discard` löscht nichts, die
        // `.key` bleibt, wie sie war — und `sessions` meldet das Verzeichnis
        // als unauflösbar, statt es einer der beiden Kennungen zuzuschlagen.
        assert!(j.read(&key).unwrap().events.is_empty());
        j.discard(&key).unwrap();
        assert!(path.exists(), ".key wurde angefasst");
        let after: KeyRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(after.local_id, "theirs");
        let found = j.sessions().unwrap();
        assert!(found.keys.is_empty());
        assert_eq!(found.unresolved, vec![j.session_dir(&key)]);
    }

    #[test]
    fn a_pre_hash_journal_directory_is_healed_on_append() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "bestand").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        let legacy = make_legacy(&j, &key);

        // Der nächste append derselben Session migriert: alter Pfad weg,
        // gehashter Pfad da, `.key` geschrieben, Sequenz lückenlos fortgesetzt.
        j.append(&key, event(EventKind::Prompt)).unwrap();
        assert!(!legacy.exists(), "Alt-Verzeichnis blieb liegen");
        let out = j.read(&key).unwrap();
        assert_eq!(out.events.len(), 2);
        assert!(out.is_complete());
        let record: KeyRecord =
            serde_json::from_slice(&fs::read(j.session_dir(&key).join(KEY_FILE)).unwrap()).unwrap();
        assert_eq!(record.local_id, "bestand");
    }

    #[test]
    fn sessions_still_finds_an_unhealed_legacy_directory_by_name() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "bestand").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        make_legacy(&j, &key);

        let found = j.sessions().unwrap();
        assert_eq!(found.keys, vec![key]);
        assert!(found.unresolved.is_empty());
    }

    #[test]
    fn read_and_discard_work_on_an_unhealed_legacy_session() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "bestand").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        let legacy = make_legacy(&j, &key);

        // Ohne den Fallback hielte `read` die Alt-Session für leer und
        // `discard` löschte nichts — der Checkpoint verarbeitete sie dann bei
        // jedem Lauf erneut.
        assert_eq!(j.read(&key).unwrap().events.len(), 1);
        j.discard(&key).unwrap();
        assert!(!legacy.exists());
        assert!(j.sessions().unwrap().keys.is_empty());
    }

    #[test]
    fn a_corrupt_key_file_is_reported_as_unresolved_not_silently_dropped() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "beschaedigt").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        let dir = j.session_dir(&key);

        // Kaputtes JSON: keine Identität mehr, aber die Events liegen da.
        fs::write(dir.join(KEY_FILE), b"kaputt").unwrap();
        let found = j.sessions().unwrap();
        assert!(found.keys.is_empty());
        assert_eq!(found.unresolved, vec![dir.clone()]);

        // Auch ein in sich widersprüchlicher Schlüssel (Hash passt nicht zum
        // Fundort) ist unauflösbar, kein Phantom-Schlüssel.
        let foreign = KeyRecord {
            version: KEY_RECORD_VERSION,
            agent: "claude-code".into(),
            local_id: "woanders".into(),
        };
        fs::write(dir.join(KEY_FILE), serde_json::to_vec(&foreign).unwrap()).unwrap();
        let found = j.sessions().unwrap();
        assert!(found.keys.is_empty());
        assert_eq!(found.unresolved, vec![dir]);
    }

    #[test]
    fn an_attacker_cannot_impersonate_a_victim_by_naming_their_own_id_after_the_victims_hash() {
        let (_tmp, j) = journal();
        let victim = SessionKey::new("claude-code", "31f3f224-victim").unwrap();
        j.append(&victim, event(EventKind::SessionStart)).unwrap();

        // Der Angreifer nennt seine Session wörtlich wie das Verzeichnis des
        // Opfers — `b3-` und Hex passieren die Zeichenprüfung anstandslos.
        let victim_dir = j.session_dir(&victim);
        let stolen_name = victim_dir.file_name().unwrap().to_str().unwrap().to_owned();
        let attacker = SessionKey::new("claude-code", stolen_name).unwrap();
        j.append(&attacker, event(EventKind::SessionStart)).unwrap();

        // Beide Sessions leben getrennt weiter: nichts migriert, nichts
        // vermischt, nichts überschrieben.
        assert_eq!(j.read(&victim).unwrap().events.len(), 1);
        assert_eq!(j.read(&attacker).unwrap().events.len(), 1);
        let record: KeyRecord =
            serde_json::from_slice(&fs::read(victim_dir.join(KEY_FILE)).unwrap()).unwrap();
        assert_eq!(record.local_id, "31f3f224-victim");
        let found = j.sessions().unwrap();
        assert_eq!(found.keys.len(), 2);
        assert!(found.unresolved.is_empty());
    }

    #[test]
    fn a_hash_shaped_local_id_is_not_confused_with_a_real_hash_bucket() {
        let (_tmp, j) = journal();
        // Ein Bestandsverzeichnis, dessen roher Name zufällig wie ein
        // Hash-Bucket aussieht: Ohne `.key` zählt der Name, nicht die Form.
        let key = SessionKey::new("claude-code", "b3-0123456789abcdef").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        make_legacy(&j, &key);

        let found = j.sessions().unwrap();
        assert_eq!(found.keys, vec![key.clone()]);
        assert_eq!(j.read(&key).unwrap().events.len(), 1);

        // Die Heilung verschiebt es an seinen echten Hash-Platz.
        j.append(&key, event(EventKind::Prompt)).unwrap();
        assert_eq!(j.read(&key).unwrap().events.len(), 2);
    }

    #[test]
    fn an_unsafe_key_error_names_the_rule_not_the_value() {
        // Ein Wert, der die Prüfung reißt, ist der verdächtigste von allen —
        // ein JWT über 128 Zeichen, ein Base64-Secret mit `+`. Genau er darf
        // nie im Fehlertext stehen, denn der wandert ins hook.log (#95).
        let jwt = format!("eyJhbGciOiJIUzI1NiJ9.{}", "x".repeat(140));
        let err = SessionKey::new("claude-code", &jwt).unwrap_err();
        assert!(!err.to_string().contains("eyJ"), "{err}");
        assert!(!err.to_string().contains("xxx"), "{err}");
        assert!(err.to_string().contains("161 Zeichen"), "{err}");
    }

    #[test]
    fn an_unresolved_directory_with_a_raw_name_is_never_named_by_it() {
        // Ein Alt-Verzeichnis (roher Token-Name), in das eine kaputte `.key`
        // geraten ist: unauflösbar — aber der gemeldete Pfad darf den Namen
        // nicht tragen, er erscheint in fsck-Ausgabe und hook.log.
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "glpat-ABCDEFGHIJ1234567890").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();
        let legacy = make_legacy(&j, &key);
        fs::write(legacy.join(KEY_FILE), b"kein json").unwrap();

        let found = j.sessions().unwrap();
        assert!(found.keys.is_empty());
        assert_eq!(found.unresolved.len(), 1);
        let shown = found.unresolved[0].to_string_lossy();
        assert!(!shown.contains("glpat"), "{shown}");
        assert!(shown.ends_with('…'), "{shown}");
    }

    #[test]
    fn read_and_discard_leave_a_foreign_bucket_alone() {
        // Dieselbe Verteidigung wie beim append, auf den kalten Pfaden: Ein
        // Bucket, dessen `.key` einer anderen Session gehört, wird weder
        // gelesen noch gelöscht — auch dann nicht, wenn er zufällig (oder
        // absichtlich) am gehashten Platz dieses Schlüssels steht.
        let (_tmp, j) = journal();
        let owner = SessionKey::new("claude-code", "echte-session").unwrap();
        j.append(&owner, event(EventKind::SessionStart)).unwrap();

        let other = SessionKey::new("claude-code", "andere-session").unwrap();
        // Fremdbesetzung simulieren: Der Bucket von `other` trägt den
        // Schlüssel von `owner`.
        let occupied = j.session_dir(&other);
        fs::create_dir_all(&occupied).unwrap();
        fs::copy(
            j.session_dir(&owner).join(KEY_FILE),
            occupied.join(KEY_FILE),
        )
        .unwrap();

        assert!(j.read(&other).unwrap().events.is_empty());
        j.discard(&other).unwrap();
        assert!(occupied.exists(), "fremder Bucket wurde geloescht");
    }

    #[test]
    fn an_empty_directory_is_a_crash_remnant_not_a_session() {
        let (_tmp, j) = journal();
        let key = SessionKey::new("claude-code", "echt").unwrap();
        j.append(&key, event(EventKind::SessionStart)).unwrap();

        // Absturzreste der Anlage: ein Bucket ohne `.key` und ohne Events,
        // und ein leeres Verzeichnis mit beliebigem Namen.
        fs::create_dir_all(j.root().join("claude-code/b3-0000000000000000")).unwrap();
        fs::create_dir_all(j.root().join("claude-code/leer")).unwrap();

        let found = j.sessions().unwrap();
        assert_eq!(found.keys, vec![key]);
        assert!(found.unresolved.is_empty());
    }

    #[test]
    fn display_redacted_hides_the_local_id_but_not_the_agent() {
        let pipeline = minds_redact::RedactionConfig::default().pipeline().unwrap();
        let key = SessionKey::new("claude-code", "glpat-ABCDEFGHIJ1234567890").unwrap();

        let shown = key.display_redacted(&pipeline);
        assert!(shown.starts_with("claude-code/"), "{shown}");
        assert!(!shown.contains("glpat-ABCDEFGHIJ"), "{shown}");

        // Eine gewöhnliche UUID bleibt lesbar — die Redaktion trifft Token,
        // nicht Diagnose-Komfort.
        let plain = SessionKey::new("claude-code", "31f3f224-f440-41ac-9244").unwrap();
        assert_eq!(
            plain.display_redacted(&pipeline),
            "claude-code/31f3f224-f440-41ac-9244"
        );
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
