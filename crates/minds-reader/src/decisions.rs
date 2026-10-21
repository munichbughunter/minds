//! Gilt eine Entscheidung noch? Zur Lesezeit aus Record und Worktree
//! gerechnet, nie gespeichert (W2/W5).
//!
//! # Die Frage der Seite
//!
//! Ein [`DecisionRecord`] sagt, was gewählt und was verworfen wurde — und
//! unter welchen Annahmen. Dieses Modul beantwortet, was der nächste Agent
//! wissen muss: **Welche Annahme gilt heute nicht mehr, und welche
//! verworfene Alternative gehört deshalb zurück auf den Tisch?**
//!
//! # Die Regel
//!
//! - Fällt eine **tragende** Annahme hinter der Verwerfung von B
//!   ([`ProbeResult::Fails`]), ist B neu zu bewerten: `reconsider`.
//! - Fällt eine tragende Annahme des **gewählten** Wegs, steht die
//!   Entscheidung selbst zur Prüfung — unabhängig von Alternativen, und
//!   das ist das stärkere Signal.
//! - Ist eine tragende Annahme nur **verändert** ([`ProbeResult::Stale`]),
//!   heißt es `re-check`, kein Vorschlag.
//! - Konnte eine tragende Probe **nicht laufen** ([`ProbeResult::Unknown`]),
//!   ist der Stand `unchecked` — nie „in Kraft": Was nicht geprüft wurde,
//!   gilt nicht als bestätigt.
//! - Eine **Randannahme** ändert den Stand nie — sie wird vermerkt. Ebenso
//!   das `rests_on` einer Alternative: vermerkt, nie ein Vorschlag.
//! - Eine Annahme **ohne Probe** fällt nie automatisch; sie wird gezeigt.
//! - Geprüft wird nur die **Spitze** einer `supersedes`-Kette; abgelöste
//!   Records sind Historie. Zeigen zwei Records auf denselben Vorgänger,
//!   löst der jüngere ab; der unterlegene bleibt eine Spitze, denn ihn löst
//!   niemand ab. Ein Zyklus hat keine Spitze — seine Records bleiben alle
//!   sichtbar. `supersedes` ist dabei eine Angabe wie `decided_by`: Wer den
//!   Store schreibt, kann jeden Record ablösen. Ein Beleg (Signatur, eine
//!   Session, die den Record nennt) ist Sache späterer Commits und muss
//!   dann vor einer unbelegten Ablösung gehen.
//!
//! # Der Record wird nicht geglaubt
//!
//! Der Record kommt aus dem Store, den der Agent schreiben kann — auch am
//! `minds`-Binary vorbei. Deshalb läuft [`DecisionRecord::check`] hier noch
//! einmal, vor jeder Probe: Ein Record, der die Prüfung nicht besteht,
//! bekommt keine Probe und steht als `unchecked` mit Grund — auch wenn er
//! `open` sagt. Und der Reader liest nur, was die Dateiliste des
//! Repositories nennt: Ein Pfad, der dort nicht steht — ignoriert, nicht
//! getrackt, außerhalb —, wird nie geöffnet, auch nicht auf Existenz
//! geprüft. Sonst wäre eine Probe ein Orakel für Dateien, die der Agent
//! selbst nicht lesen darf (`.env` hinter einer Sandbox-Regel).
//!
//! # Die Proben und ihr Budget
//!
//! Drei Arten, alle deterministisch, keine ausgeführten Kommandos: ein
//! Muster in einer Datei oder einem Glob, ein Pfad, eine Blob-Id. Der
//! Worktree kommt als [`Worktree`] herein — die CLI reicht das Repository,
//! Tests einen [`MemoryTree`]. Was die Probe nicht entscheiden kann
//! (Muster ungültig, Datei unlesbar, zu groß, zu viele Dateien, Budget
//! erschöpft), ist [`ProbeResult::Unknown`] mit Grund — nie still „gilt".
//!
//! Budgets gibt es auf drei Ebenen: je Probe ([`MAX_PROBE_BYTES`]), je
//! Record ([`MAX_RECORD_BYTES`], [`RECORD_GLOB_BUDGET`]) und je Lauf
//! ([`MAX_RUN_BYTES`], [`crate::scope::RUN_BUDGET`], [`MAX_RUN_TIME`]). Ein
//! feindlicher Record nimmt so vor allem sich selbst die Probe; was vom
//! Lauf übrig ist, bekommen die jüngsten Records zuerst — und nur die
//! jüngsten [`MAX_ASSESSED`] überhaupt. `at` ist dabei eine Angabe des
//! Schreibers: Records mit erfundenem jungem Datum können den älteren das
//! Lauf-Budget nehmen (die werden dann `unchecked`, nie „in Kraft"). Das
//! ist die Vertrauensfrage unbelegter Records, die der Beleg löst, nicht
//! das Budget. Ein regulärer
//! Ausdruck ist in seiner Größe eng begrenzt ([`REGEX_SIZE_LIMIT`], beim
//! Übersetzen fortlaufend geprüft — das ist die Garantie); Unicode-Klassen
//! sind nur abgeschaltet (`\w`, `\d`, `\s` sind ASCII — Code ist ASCII),
//! ein `(?u)` im Muster schaltet sie ein und scheitert dann am Limit. Die
//! Muster, um die es geht (`\bfork\(`, `windows-sys`), brauchen Bytes; ein
//! Muster, das Kilobytes braucht, ist eines, das den Matcher beschäftigen
//! soll. Was trotzdem teuer ist, begrenzt die Frist.
//!
//! # Was Teil 2 einlösen muss
//!
//! Der Record erreicht den Store nur redigiert (`check → redact → id →
//! store`, siehe `minds_core::decision`), auf Typ-Ebene erzwungen wie bei
//! `RedactedSession`. Ersetzt die Redaction in einem Muster etwas, läuft
//! die Probe mit verändertem Muster — sichtbar als `Fails` oder `Unknown`,
//! nie still als „gilt"; `minds decide` meldet das beim Ablegen.

use std::cell::{Cell, OnceCell};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use minds_core::ContentHash;
use minds_core::decision::{
    DecisionRecord, Probe, Reason, Weight, is_control_or_format, is_glob, is_plain_segment,
    is_repo_relative,
};

use crate::scope::{Glob, MAX_PATH, RUN_BUDGET};
use crate::text::sanitize;

/// So viele Bytes liest eine Muster-Probe je Datei höchstens.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// So viele Bytes liest eine Muster-Probe insgesamt höchstens.
pub const MAX_PROBE_BYTES: u64 = 8 * 1024 * 1024;

/// So viele Bytes lesen die Proben eines Records zusammen höchstens.
pub const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

/// So viele Bytes liest ein ganzer Lauf ([`assess_all`]) höchstens.
pub const MAX_RUN_BYTES: u64 = 32 * 1024 * 1024;

/// So viele Glob-Abgleichschritte bekommt ein Record.
pub const RECORD_GLOB_BUDGET: u64 = RUN_BUDGET / 8;

/// So lange darf ein Lauf Proben auswerten; danach ist jede weitere
/// Probe „nicht geprüft". Der Brief läuft beim Start jeder Session.
/// Geprüft wird vor jeder Datei: Ein laufender Abgleich wird nicht
/// unterbrochen, die Frist kann um einen Datei-Abgleich überzogen werden
/// (bei [`MAX_FILE_BYTES`] und [`REGEX_SIZE_LIMIT`] gemessen bis ~0,5 s
/// für Muster ohne Vorfilter, `[ab]*a[ab]{30}[^ab]`).
pub const MAX_RUN_TIME: Duration = Duration::from_secs(3);

/// So viele Dateien eines Globs prüft eine Muster-Probe höchstens.
pub const MAX_GLOB_FILES: usize = 10_000;

/// So viele Records bekommen in einem Lauf ([`assess_all`]) ihre Proben —
/// die jüngsten; die übrigen werden gezeigt, aber nicht geprüft.
pub const MAX_ASSESSED: usize = 512;

/// Obergrenze für den kompilierten Ausdruck einer Muster-Probe (NFA und
/// Lazy-DFA je) — eng, siehe Modul-Doku. Gemessen: `\bfork\(`,
/// `fn\s+\w+\s*\(` und `\b(TODO|FIXME)\b.*\d{4}` passen (ASCII-Klassen),
/// `[ab]*a[ab]{100}c` nicht mehr; `[ab]{50}` kostet je 4 MiB noch ~0,3 s,
/// was die Frist des Laufs trägt.
pub const REGEX_SIZE_LIMIT: usize = 4 * 1024;

/// Warum der Worktree eine Datei nicht liefern konnte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// Es gibt sie nicht.
    #[error("missing")]
    Missing,
    /// Es gibt sie, aber sie ist nicht lesbar (Rechte, I/O, ein Symlink,
    /// der den Vertrag bricht).
    #[error("not readable")]
    Unreadable,
    /// Sie ist größer als das übergebene Maximum.
    #[error("too large")]
    TooLarge,
}

/// Die Sicht auf den Worktree, gegen den Proben laufen.
///
/// # Der Vertrag
///
/// Pfade sind repo-relativ mit `/` ([`is_repo_relative`] — der Reader
/// reicht nichts anderes durch), und der Reader liest **nur Pfade, die
/// [`files`](Self::files) nennt**. Eine Implementierung
///
/// - nennt in `files` nur **getrackte** Dateien des Repositories — nicht
///   „nicht ignoriert": `.gitignore` ist kein Ersatz für die Regeln, die
///   dem Agenten das Lesen verbieten, und die drei Proben brauchen nur
///   getrackten Inhalt —, nie `.git`, nichts außerhalb; Namen mit
///   Steuerzeichen, leerem, `..`- oder `.git`-Segment machen jede
///   Glob-Probe unentscheidbar ([`is_listable`]);
/// - liefert in `read`/`blob_id` **nie den Inhalt hinter einem Symlink**,
///   gleich wohin er zeigt: Ein getrackter Link `probe -> .env` steht in
///   der Liste, `.env` nicht — folgte `read` dem Link, wäre die Probe das
///   Orakel, das die Liste verhindern soll. Jede Komponente wird ohne
///   Folgen geprüft (`lstat`, `O_NOFOLLOW`), ein Link ist
///   [`ReadError::Unreadable`]. Am einfachsten erfüllt das, wer aus dem
///   Index oder dem `HEAD`-Baum liest: Dort ist der Blob eines Links sein
///   Zieltext, nie der Inhalt;
/// - unterscheidet „fehlt" von „nicht lesbar": Ein I/O-Fehler, eine
///   Berechtigungsstörung, ein abgelehnter Symlink ist
///   [`ReadError::Unreadable`], nie [`ReadError::Missing`] — sonst würde
///   eine Annahme „kommt nicht vor" durch eine Störung bestätigt;
/// - liefert in `read` nie mehr als `max` Bytes (sonst wertet der Reader
///   die Probe nicht), und hasht in `blob_id` nie mehr als
///   [`MAX_FILE_BYTES`] von der Platte (darüber [`ReadError::TooLarge`]) —
///   aus dem Index kommt die Id ohne Hashen;
/// - vergleicht Namen als Bytes, wie Git: Ein Pfad in anderer
///   Unicode-Normalisierung (NFC/NFD) oder Schreibung ist ein anderer Name
///   — auf macOS kann ein `Path`-Probe deshalb „fehlt" sagen, wo die Platte
///   die Datei kennt;
/// - **verkettet einen Listennamen nie mit einem Plattenpfad**, um ihn zu
///   öffnen: Der Inhalt gehört zum Listeneintrag (Index- oder
///   `HEAD`-Blob). Unter Windows ersetzte `Path::join` bei `C:/…` die
///   Wurzel, und `GIT~1` erreichte `.git`.
pub trait Worktree {
    /// Alle Dateien des Repositories, die eine Probe sehen darf.
    fn files(&self) -> Result<Vec<String>, ReadError>;
    /// Der Inhalt einer Datei, höchstens `max` Bytes — darüber
    /// [`ReadError::TooLarge`], ohne den Rest zu lesen.
    fn read(&self, path: &str, max: usize) -> Result<Vec<u8>, ReadError>;
    /// Die Git-Blob-Id des aktuellen Inhalts (wie `git hash-object
    /// --no-filters`), 40 oder 64 Hex-Zeichen, klein — so steht sie im
    /// Record, und die Probe vergleicht Zeichen.
    fn blob_id(&self, path: &str) -> Result<String, ReadError>;
}

/// Ein Worktree im Speicher — für Tests und Golden-Läufe.
#[derive(Debug, Default, Clone)]
pub struct MemoryTree {
    files: BTreeMap<String, (Vec<u8>, Option<String>)>,
    unreadable: BTreeSet<String>,
    hidden: BTreeMap<String, Vec<u8>>,
    listing_fails: bool,
}

impl MemoryTree {
    /// Leer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Legt eine Datei ab; die Blob-Id ist unbekannt.
    pub fn with(mut self, path: &str, content: &[u8]) -> Self {
        self.files.insert(path.to_owned(), (content.to_vec(), None));
        self
    }

    /// Legt eine Datei mit bekannter Blob-Id ab.
    pub fn with_blob(mut self, path: &str, content: &[u8], blob: &str) -> Self {
        self.files
            .insert(path.to_owned(), (content.to_vec(), Some(blob.to_owned())));
        self
    }

    /// Legt eine Datei ab, die existiert, aber nicht lesbar ist.
    pub fn with_unreadable(mut self, path: &str) -> Self {
        self.unreadable.insert(path.to_owned());
        self
    }

    /// Legt eine Datei ab, die auf der Platte liegt, aber nicht in der
    /// Dateiliste steht (ignoriert, nicht getrackt) — lesbar, wenn jemand
    /// sie anfasst.
    pub fn with_hidden(mut self, path: &str, content: &[u8]) -> Self {
        self.hidden.insert(path.to_owned(), content.to_vec());
        self
    }

    /// Die Dateiliste schlägt fehl.
    pub fn without_listing(mut self) -> Self {
        self.listing_fails = true;
        self
    }
}

impl Worktree for MemoryTree {
    fn files(&self) -> Result<Vec<String>, ReadError> {
        if self.listing_fails {
            return Err(ReadError::Unreadable);
        }
        Ok(self
            .files
            .keys()
            .chain(self.unreadable.iter())
            .cloned()
            .collect())
    }

    fn read(&self, path: &str, max: usize) -> Result<Vec<u8>, ReadError> {
        if self.unreadable.contains(path) {
            return Err(ReadError::Unreadable);
        }
        let content = match self.files.get(path) {
            Some((content, _)) => content,
            None => self.hidden.get(path).ok_or(ReadError::Missing)?,
        };
        if content.len() > max {
            return Err(ReadError::TooLarge);
        }
        Ok(content.clone())
    }

    fn blob_id(&self, path: &str) -> Result<String, ReadError> {
        if self.unreadable.contains(path) {
            return Err(ReadError::Unreadable);
        }
        let (_, blob) = self.files.get(path).ok_or(ReadError::Missing)?;
        blob.clone().ok_or(ReadError::Unreadable)
    }
}

/// Was eine Probe ergab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    /// Die Annahme gilt.
    Holds,
    /// Die Annahme gilt nicht mehr.
    Fails,
    /// Das Material hat sich seit der Entscheidung geändert; ob die Annahme
    /// noch gilt, kann die Maschine nicht sagen.
    Stale,
    /// Keine Probe — die Annahme ist nicht prüfbar.
    Unverifiable,
    /// Die Probe konnte nicht entscheiden, aus diesem Grund.
    Unknown(&'static str),
}

impl ProbeResult {
    /// Das Wort in der Anzeige.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProbeResult::Holds => "holds",
            ProbeResult::Fails => "no longer holds",
            ProbeResult::Stale => "changed since",
            ProbeResult::Unverifiable => "not checkable",
            ProbeResult::Unknown(_) => "not checked",
        }
    }
}

/// Ein Byte- und ein Glob-Budget — je Record und je Lauf eines.
struct Budget {
    bytes: Cell<u64>,
    glob: Cell<u64>,
}

impl Budget {
    fn new(bytes: u64, glob: u64) -> Self {
        Self {
            bytes: Cell::new(bytes),
            glob: Cell::new(glob),
        }
    }

    fn record() -> Self {
        Self::new(MAX_RECORD_BYTES, RECORD_GLOB_BUDGET)
    }
}

/// Die Dateiliste, einmal geprüft: sortiert die Namen, die [`is_listable`]
/// bestehen; die anderen daneben — sie machen jede Glob-Probe
/// unentscheidbar, und eine Pfad-Probe dort, wo sie träfen.
struct Listing {
    files: Vec<String>,
    unlistable: Vec<String>,
}

/// Ein Lauf über den Worktree: die Dateiliste einmal, das Lauf-Budget und
/// die Frist für alle Records zusammen.
struct Run<'a> {
    tree: &'a dyn Worktree,
    files: OnceCell<Result<Listing, ReadError>>,
    budget: Budget,
    deadline: Instant,
}

impl<'a> Run<'a> {
    fn new(tree: &'a dyn Worktree) -> Self {
        Self::with_deadline(tree, Instant::now() + MAX_RUN_TIME)
    }

    fn with_deadline(tree: &'a dyn Worktree, deadline: Instant) -> Self {
        Self {
            tree,
            files: OnceCell::new(),
            budget: Budget::new(MAX_RUN_BYTES, RUN_BUDGET),
            deadline,
        }
    }

    fn listing(&self) -> Result<&Listing, ReadError> {
        match self.files.get_or_init(|| {
            self.tree.files().map(|names| {
                let (mut files, unlistable): (Vec<String>, Vec<String>) =
                    names.into_iter().partition(|f| is_listable(f));
                files.sort();
                files.dedup();
                Listing { files, unlistable }
            })
        }) {
            Ok(listing) => Ok(listing),
            Err(why) => Err(*why),
        }
    }

    /// Steht der Pfad in der Dateiliste? Ein unlistbarer Name kann es nie
    /// sein — der Pfad besteht [`is_repo_relative`], und das ist strenger.
    fn lists(&self, path: &str) -> Result<bool, ReadError> {
        Ok(self
            .listing()?
            .files
            .binary_search_by(|f| f.as_str().cmp(path))
            .is_ok())
    }

    /// Steht der Pfad oder etwas darunter in der Dateiliste? Liegt ein
    /// unlistbarer Name darunter, ist das nicht entscheidbar — er wäre
    /// der Treffer.
    fn has(&self, path: &str) -> Result<bool, ReadError> {
        let listing = self.listing()?;
        let from = listing.files.partition_point(|f| f.as_str() < path);
        let below = |f: &String| {
            f.as_str() == path || (f.starts_with(path) && f[path.len()..].starts_with('/'))
        };
        if listing.files[from..]
            .iter()
            .take_while(|f| f.starts_with(path))
            .any(below)
        {
            return Ok(true);
        }
        if listing.unlistable.iter().any(below) {
            return Err(ReadError::Unreadable);
        }
        Ok(false)
    }

    fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline
    }
}

/// Darf ein Name aus der Dateiliste in einer Probe mitlaufen? Etwas
/// weniger streng als [`is_repo_relative`] (das für Pfade aus dem Record
/// gilt): Ein `\` oder `:` in einem Dateinamen ist unter Linux und macOS
/// legal und kein Angriff — unter Windows dagegen ein Laufwerk, ein
/// NTFS-Stream oder ein Trenner, und dort lehnt die Liste ihn ab. Die
/// `.git`-Aliasse gelten überall: Der Index ist agentseitig schreibbar,
/// und `GIT~1/config` darin wäre unter Windows `.git/config`.
pub fn is_listable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PATH
        && !name.starts_with('/')
        && !name.chars().any(is_control_or_format)
        && (!cfg!(windows) || !(name.contains('\\') || name.contains(':')))
        && name.split('/').all(is_plain_segment)
}

/// Die Records, die auf einem Zyklus liegen — in einem Durchgang über den
/// Nachfolger-Graphen (jeder Record hat höchstens einen Nachfolger): Jeder
/// Knoten wird einmal betreten, egal wie lang die Ketten sind
/// (O(n log n) mit den Baum-Maps, nie quadratisch).
fn cycles(successor: &BTreeMap<ContentHash, ContentHash>) -> BTreeSet<ContentHash> {
    // 0 = unbesucht, 1 = auf dem aktuellen Weg, 2 = abgeschlossen.
    let mut state: BTreeMap<&ContentHash, u8> = BTreeMap::new();
    let mut cyclic: BTreeSet<ContentHash> = BTreeSet::new();
    for start in successor.keys() {
        if state.contains_key(start) {
            continue;
        }
        let mut path: Vec<&ContentHash> = Vec::new();
        let mut at = start;
        loop {
            match state.get(at).copied() {
                Some(2) => break,
                Some(1) => {
                    // Zurück am eigenen Weg: ab hier ist alles Zyklus.
                    let from = path.iter().position(|p| *p == at).unwrap_or(0);
                    cyclic.extend(path[from..].iter().map(|p| (*p).clone()));
                    break;
                }
                _ => {
                    state.insert(at, 1);
                    path.push(at);
                    match successor.get(at) {
                        Some(next) => at = next,
                        None => break,
                    }
                }
            }
        }
        for p in path {
            state.insert(p, 2);
        }
    }
    cyclic
}

/// Wertet eine Probe gegen den Worktree aus. `None` heißt: keine Probe.
/// Für einen einzelnen Aufruf mit eigener Frist und eigenem Budget — die
/// CLI nimmt über viele Records [`assess_all`], das beides teilt; eine
/// Schleife über `evaluate` oder [`assess`] hätte kein Gesamtbudget.
pub fn evaluate(probe: Option<&Probe>, tree: &dyn Worktree) -> ProbeResult {
    evaluate_in(probe, &Run::new(tree), &Budget::record())
}

fn evaluate_in(probe: Option<&Probe>, run: &Run<'_>, record: &Budget) -> ProbeResult {
    let Some(probe) = probe else {
        return ProbeResult::Unverifiable;
    };
    if run.out_of_time() {
        return ProbeResult::Unknown("time budget exhausted");
    }
    match probe {
        Probe::Path { path, exists } => {
            if !is_repo_relative(path) {
                return ProbeResult::Unknown("path is not repo-relative");
            }
            match run.has(path) {
                Ok(now) if now == *exists => ProbeResult::Holds,
                Ok(_) => ProbeResult::Fails,
                Err(_) => ProbeResult::Unknown("files not listable"),
            }
        }
        Probe::Blob { path, blob } => {
            if !is_repo_relative(path) {
                return ProbeResult::Unknown("path is not repo-relative");
            }
            match run.lists(path) {
                // Gelöscht (oder nicht mehr getrackt) ist eine Änderung.
                Ok(false) => return ProbeResult::Stale,
                Ok(true) => {}
                Err(_) => return ProbeResult::Unknown("files not listable"),
            }
            match run.tree.blob_id(path) {
                Ok(now) if now == *blob => ProbeResult::Holds,
                Ok(_) | Err(ReadError::Missing) => ProbeResult::Stale,
                Err(ReadError::TooLarge) => ProbeResult::Unknown("file too large"),
                Err(ReadError::Unreadable) => ProbeResult::Unknown("file not readable"),
            }
        }
        Probe::Pattern {
            path,
            glob,
            must_match,
            must_not_match,
        } => {
            if [must_match, must_not_match]
                .into_iter()
                .flatten()
                .any(|p| p.is_empty() || p.len() > minds_core::decision::MAX_TEXT)
            {
                return ProbeResult::Unknown("pattern is empty or too long");
            }
            let must = match must_match.as_deref().map(compile) {
                Some(Err(why)) => return ProbeResult::Unknown(why),
                Some(Ok(regex)) => Some(regex),
                None => None,
            };
            let must_not = match must_not_match.as_deref().map(compile) {
                Some(Err(why)) => return ProbeResult::Unknown(why),
                Some(Ok(regex)) => Some(regex),
                None => None,
            };
            if must.is_none() && must_not.is_none() {
                return ProbeResult::Unknown("probe without pattern");
            }
            let paths: Vec<&str> = match (path, glob) {
                (Some(_), Some(_)) => return ProbeResult::Unknown("probe has both path and glob"),
                (None, None) => return ProbeResult::Unknown("probe without path or glob"),
                (Some(path), None) => {
                    if !is_repo_relative(path) {
                        return ProbeResult::Unknown("path is not repo-relative");
                    }
                    match run.lists(path) {
                        Ok(true) => vec![path.as_str()],
                        Ok(false) => return ProbeResult::Unknown("path not in the repository"),
                        Err(_) => return ProbeResult::Unknown("files not listable"),
                    }
                }
                (None, Some(glob)) => {
                    if !is_glob(glob) {
                        return ProbeResult::Unknown("glob is not repo-relative");
                    }
                    let glob = Glob::new(glob);
                    let listing = match run.listing() {
                        Ok(listing) => listing,
                        Err(_) => return ProbeResult::Unknown("files not listable"),
                    };
                    if !listing.unlistable.is_empty() {
                        return ProbeResult::Unknown("a file name is not listable");
                    }
                    let mut hits: Vec<&str> = Vec::new();
                    for (n, file) in listing.files.iter().enumerate() {
                        if n % 1024 == 1023 && run.out_of_time() {
                            return ProbeResult::Unknown("time budget exhausted");
                        }
                        let granted = record.glob.get().min(run.budget.glob.get());
                        if granted == 0 {
                            return ProbeResult::Unknown("glob budget exhausted");
                        }
                        let budget = Cell::new(granted);
                        let hit = glob.matches_within(file, &budget);
                        let used = granted - budget.get();
                        record.glob.set(record.glob.get().saturating_sub(used));
                        run.budget
                            .glob
                            .set(run.budget.glob.get().saturating_sub(used));
                        match hit {
                            Some(true) => hits.push(file),
                            Some(false) => {}
                            None => return ProbeResult::Unknown("glob budget exhausted"),
                        }
                        if hits.len() > MAX_GLOB_FILES {
                            return ProbeResult::Unknown("too many files");
                        }
                    }
                    // „Kommt in keiner Datei vor" über null Dateien wäre
                    // leer wahr: Das Subjekt der Annahme ist weg (Verzeichnis
                    // verschoben, Dateien umbenannt). Ein Muster, das
                    // vorkommen *muss*, kommt dagegen wirklich nicht vor.
                    if hits.is_empty() && must_match.is_none() {
                        return ProbeResult::Unknown("glob matches no file");
                    }
                    hits
                }
            };
            let mut probe_bytes_left = MAX_PROBE_BYTES;
            let mut any_must = false;
            for path in paths {
                if run.out_of_time() {
                    return ProbeResult::Unknown("time budget exhausted");
                }
                let allowed = probe_bytes_left
                    .min(record.bytes.get())
                    .min(run.budget.bytes.get())
                    .min(MAX_FILE_BYTES as u64) as usize;
                // Ein Byte mehr verlangt, als erlaubt: Liefert der Worktree
                // genau `allowed + 1`, war die Datei zu groß — auch bei einer
                // Implementierung, die stillschweigend kürzt statt
                // `TooLarge` zu melden.
                let content = match run.tree.read(path, allowed + 1) {
                    Ok(content) if content.len() > allowed + 1 => {
                        return ProbeResult::Unknown("worktree returned more than asked");
                    }
                    Ok(content) if content.len() > allowed => {
                        return ProbeResult::Unknown(if allowed < MAX_FILE_BYTES {
                            "byte budget exhausted"
                        } else {
                            "file too large"
                        });
                    }
                    Ok(content) => content,
                    // Gelistet, aber weg: Das Subjekt der Annahme fehlt —
                    // wie bei einem Glob ohne Treffer nicht entscheidbar.
                    Err(ReadError::Missing) => {
                        return ProbeResult::Unknown("file missing from the worktree");
                    }
                    Err(ReadError::Unreadable) => return ProbeResult::Unknown("file not readable"),
                    Err(ReadError::TooLarge) if allowed < MAX_FILE_BYTES => {
                        return ProbeResult::Unknown("byte budget exhausted");
                    }
                    Err(ReadError::TooLarge) => return ProbeResult::Unknown("file too large"),
                };
                let len = content.len() as u64;
                probe_bytes_left = probe_bytes_left.saturating_sub(len);
                record.bytes.set(record.bytes.get().saturating_sub(len));
                run.budget
                    .bytes
                    .set(run.budget.bytes.get().saturating_sub(len));
                if let Some(regex) = &must_not {
                    if regex.is_match(&content) {
                        return ProbeResult::Fails;
                    }
                }
                if let Some(regex) = &must {
                    if regex.is_match(&content) {
                        any_must = true;
                        // „Kommt in mindestens einer Datei vor" ist mit dem
                        // ersten Treffer bewiesen — ohne `must_not` ist der
                        // Rest nur Budget.
                        if must_not.is_none() {
                            return ProbeResult::Holds;
                        }
                    }
                }
            }
            if must.is_some() && !any_must {
                ProbeResult::Fails
            } else {
                ProbeResult::Holds
            }
        }
    }
}

fn compile(pattern: &str) -> Result<regex::bytes::Regex, &'static str> {
    regex::bytes::RegexBuilder::new(pattern)
        .unicode(false)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| "pattern invalid or too large")
}

/// Eine Annahme mit dem Ergebnis ihrer Probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumptionStatus {
    /// Die Kennung, entschärft.
    pub id: String,
    /// Die Aussage, entschärft.
    pub statement: String,
    /// Ob sie eine Probe trägt.
    pub probed: bool,
    /// Das Ergebnis.
    pub result: ProbeResult,
}

/// Der Stand einer Entscheidung heute. Die Reihenfolge ist die
/// Dringlichkeit — [`assess_all`] sortiert danach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// Die Wahl liegt beim Menschen.
    Open,
    /// Eine tragende Annahme gilt nicht mehr: neu bewerten.
    Review,
    /// Eine tragende Annahme hat sich verändert: ansehen.
    ReCheck,
    /// Eine tragende Probe konnte nicht laufen, oder der Record selbst
    /// besteht die Prüfung nicht: nicht bestätigt.
    Unchecked,
    /// Keine tragende Annahme ist gefallen, verändert oder ungeprüft.
    InForce,
}

impl Standing {
    /// Das Wort in der Anzeige.
    pub fn as_str(&self) -> &'static str {
        match self {
            Standing::Open => "open",
            Standing::Review => "re-evaluate",
            Standing::ReCheck => "re-check",
            Standing::Unchecked => "unchecked",
            Standing::InForce => "in force",
        }
    }
}

/// Eine Alternative, die zurück auf den Tisch gehört.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconsider {
    /// Der Ansatz, entschärft.
    pub alternative: String,
    /// Die tragenden Annahmen hinter seiner Verwerfung, die nicht mehr
    /// gelten.
    pub fallen: Vec<String>,
}

/// Eine Entscheidung mit ihrem heutigen Stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// Die Record-Id.
    pub id: ContentHash,
    /// Der Record, wie gespeichert — **roh**: Thema, Ansätze und Aussagen
    /// darin sind fremder Text. Zum Anzeigen gibt es [`topic`](Self::topic),
    /// [`chosen`](Self::chosen), [`assumptions`](Self::assumptions) und
    /// [`reconsider`](Self::reconsider); alles andere erst durch
    /// [`crate::text::sanitize`].
    pub record: DecisionRecord,
    /// Das Thema, entschärft.
    pub topic: String,
    /// Der gewählte Weg, entschärft — `None` bei einem offenen Record.
    pub chosen: Option<String>,
    /// Die Alternativen bzw. Optionen, entschärft, in Record-Reihenfolge.
    pub alternatives: Vec<String>,
    /// Jede Annahme mit Probenergebnis, in Record-Reihenfolge.
    pub assumptions: Vec<AssumptionStatus>,
    /// Der Stand.
    pub standing: Standing,
    /// Tragende Annahmen des gewählten Wegs, die nicht mehr gelten.
    pub fallen: Vec<String>,
    /// Tragende Annahmen (Weg oder Verwerfung), die sich verändert haben.
    pub stale: Vec<String>,
    /// Tragende Annahmen (Weg oder Verwerfung), deren Probe nicht laufen
    /// konnte.
    pub unchecked: Vec<String>,
    /// Alternativen, deren tragende Verwerfungsgründe gefallen sind.
    pub reconsider: Vec<Reconsider>,
    /// Vermerke, die den Stand nicht ändern: gefallene oder veränderte
    /// Randannahmen, das `rests_on` von Alternativen, ein Record, der die
    /// Prüfung nicht besteht.
    pub notes: Vec<String>,
    /// Der Record, der diesen abgelöst hat — `None` an der Spitze der
    /// Kette.
    pub superseded_by: Option<ContentHash>,
}

impl Assessment {
    /// Steht dieser Record an der Spitze seiner Kette?
    pub fn is_head(&self) -> bool {
        self.superseded_by.is_none()
    }

    /// Der Stand in einem Satz — dasselbe Vokabular in Brief, CLI und TUI.
    pub fn headline(&self) -> String {
        match self.standing {
            Standing::Open => format!(
                "open — waiting for a human · {} option{}",
                self.record.alternatives.len(),
                plural(self.record.alternatives.len())
            ),
            Standing::Review => {
                if !self.fallen.is_empty() {
                    format!(
                        "re-evaluate: {} no longer {} (the chosen path rests on {})",
                        self.fallen.join(", "),
                        hold(self.fallen.len()),
                        if self.fallen.len() == 1 { "it" } else { "them" }
                    )
                } else if let Some(first) = self.reconsider.first() {
                    let more = self.reconsider.len() - 1;
                    format!(
                        "re-evaluate: {} no longer {} → reconsider {}{}",
                        first.fallen.join(", "),
                        hold(first.fallen.len()),
                        first.alternative,
                        if more > 0 {
                            format!(" (+{more} more)")
                        } else {
                            String::new()
                        }
                    )
                } else {
                    "re-evaluate".to_owned()
                }
            }
            Standing::ReCheck => format!(
                "re-check: {} changed since the decision",
                self.stale.join(", ")
            ),
            Standing::Unchecked => {
                if self.unchecked.is_empty() {
                    "unchecked: the record does not pass the check".to_owned()
                } else {
                    format!(
                        "unchecked: {} could not be probed",
                        self.unchecked.join(", ")
                    )
                }
            }
            Standing::InForce => {
                let count = |f: fn(&AssumptionStatus) -> bool| {
                    self.assumptions.iter().filter(|a| f(a)).count()
                };
                let holds = count(|a| a.result == ProbeResult::Holds);
                let mut line = String::from("in force");
                // Was hier fiel oder sich änderte, trug keine Entscheidung
                // (Randannahme, `rests_on` einer Alternative, unverbunden)
                // — „noted", nicht „marginal": Das Gewicht steht am Record.
                for (n, word) in [
                    (holds, if holds == 1 { "holds" } else { "hold" }),
                    (count(|a| a.result == ProbeResult::Fails), "fell (noted)"),
                    (count(|a| a.result == ProbeResult::Stale), "changed (noted)"),
                    (
                        count(|a| matches!(a.result, ProbeResult::Unknown(_))),
                        "not checked",
                    ),
                    (count(|a| !a.probed), "not checkable"),
                ] {
                    if n > 0 {
                        line.push_str(&format!(" · {n} {word}"));
                    }
                }
                line
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn hold(n: usize) -> &'static str {
    if n == 1 { "holds" } else { "hold" }
}

/// Bewertet einen Record gegen den Worktree. Die Kette (`superseded_by`)
/// kennt nur [`assess_all`].
pub fn assess(id: ContentHash, record: DecisionRecord, tree: &dyn Worktree) -> Assessment {
    assess_in(id, record, &Run::new(tree), true)
}

/// `probes == false`: Der Record liegt jenseits von [`MAX_ASSESSED`] — er
/// wird gezeigt, seine Proben laufen nicht.
fn assess_in(id: ContentHash, record: DecisionRecord, run: &Run<'_>, probes: bool) -> Assessment {
    let invalid = record.check().err();
    let skipped = invalid.is_none() && !probes;
    let budget = Budget::record();
    let assumptions: Vec<AssumptionStatus> = record
        .assumptions
        .iter()
        .map(|assumption| AssumptionStatus {
            id: sanitize(&assumption.id),
            statement: sanitize(&assumption.statement),
            probed: assumption.probe.is_some(),
            // Eine Annahme ohne Probe ist nicht prüfbar — unabhängig davon,
            // ob der Record gültig ist oder im Lauf noch an der Reihe.
            result: match (&assumption.probe, &invalid, skipped) {
                (None, _, _) => ProbeResult::Unverifiable,
                (Some(_), Some(_), _) => ProbeResult::Unknown("record invalid"),
                (Some(_), None, true) => ProbeResult::Unknown("too many records in this run"),
                (Some(probe), None, false) => evaluate_in(Some(probe), run, &budget),
            },
        })
        .collect();
    let result_of = |id: &str| -> ProbeResult {
        record
            .assumptions
            .iter()
            .position(|a| a.id == id)
            .map_or(ProbeResult::Unknown("unknown assumption"), |i| {
                assumptions[i].result
            })
    };

    let mut fallen: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    let mut unchecked: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut reconsider: Vec<Reconsider> = Vec::new();
    if let Some(why) = &invalid {
        notes.push(sanitize(&format!("record does not pass the check: {why}")));
    }
    if skipped {
        notes.push(format!(
            "not probed: more than {MAX_ASSESSED} records in this run, the youngest were probed"
        ));
    }

    // Eine Kantenliste durchgehen: Tragende sammeln sich in `fallen_here`
    // (Fails), `stale`, `unchecked`; Randannahmen werden vermerkt. Mit
    // `decisive == false` (das `rests_on` einer Alternative) wird alles
    // nur vermerkt.
    let mut walk =
        |reasons: &[Reason], what: &str, decisive: bool, fallen_here: &mut Vec<String>| {
            for reason in reasons {
                let id = sanitize(&reason.assumption);
                let result = result_of(&reason.assumption);
                match (reason.weight, result) {
                    (Weight::LoadBearing, ProbeResult::Fails) if decisive => {
                        if !fallen_here.contains(&id) {
                            fallen_here.push(id);
                        }
                    }
                    (Weight::LoadBearing, ProbeResult::Stale) if decisive => {
                        if !stale.contains(&id) {
                            stale.push(id);
                        }
                    }
                    (Weight::LoadBearing, ProbeResult::Unknown(why)) if decisive => {
                        if !unchecked.contains(&id) {
                            unchecked.push(id.clone());
                        }
                        notes.push(format!("{id} not checked ({why}) — {what}"));
                    }
                    (weight, ProbeResult::Fails) => {
                        notes.push(format!(
                            "{id} no longer holds ({}) — {what}",
                            weight.as_str()
                        ));
                    }
                    (weight, ProbeResult::Stale) => {
                        notes.push(format!(
                            "{id} changed since the decision ({}) — {what}",
                            weight.as_str()
                        ));
                    }
                    (weight, ProbeResult::Unknown(why)) => {
                        notes.push(format!(
                            "{id} not checked ({why}, {}) — {what}",
                            weight.as_str()
                        ));
                    }
                    _ => {}
                }
            }
        };

    // Ein Record, der die Prüfung nicht besteht, bekommt keine Kanten
    // bewertet — jede Kante hieße sonst „nicht geprüft", und das steht
    // schon einmal in den Vermerken.
    if invalid.is_none() {
        if let Some(chosen) = &record.chosen {
            walk(
                &chosen.rests_on,
                "the chosen path rests on it",
                true,
                &mut fallen,
            );
        }
        for alternative in &record.alternatives {
            let approach = sanitize(&alternative.approach);
            let mut fallen_here: Vec<String> = Vec::new();
            walk(
                &alternative.rejected_because,
                &format!("behind rejecting {approach}"),
                true,
                &mut fallen_here,
            );
            if !fallen_here.is_empty() {
                reconsider.push(Reconsider {
                    alternative: approach.clone(),
                    fallen: fallen_here,
                });
            }
            walk(
                &alternative.rests_on,
                &format!("{approach} rests on it"),
                false,
                &mut Vec::new(),
            );
        }
    }

    // Annahmen, auf die keine Kante zeigt, zählen für keinen Stand — aber
    // was an ihnen fiel oder sich änderte, steht in den Vermerken, damit
    // die Zahlen der Headline erklärt sind.
    if invalid.is_none() {
        let referenced: BTreeSet<&str> = record
            .chosen
            .iter()
            .flat_map(|c| c.rests_on.iter())
            .chain(
                record
                    .alternatives
                    .iter()
                    .flat_map(|a| a.rejected_because.iter().chain(a.rests_on.iter())),
            )
            .map(|r| r.assumption.as_str())
            .collect();
        for (assumption, status) in record.assumptions.iter().zip(&assumptions) {
            if referenced.contains(assumption.id.as_str()) {
                continue;
            }
            let what = match status.result {
                ProbeResult::Fails => "no longer holds",
                ProbeResult::Stale => "changed since the decision",
                ProbeResult::Unknown(_) => "not checked",
                _ => continue,
            };
            notes.push(format!("{} {what} — not tied to any path", status.id));
        }
    }

    let standing = if invalid.is_some() {
        Standing::Unchecked
    } else if record.open {
        Standing::Open
    } else if !fallen.is_empty() || !reconsider.is_empty() {
        Standing::Review
    } else if !stale.is_empty() {
        Standing::ReCheck
    } else if !unchecked.is_empty() {
        Standing::Unchecked
    } else {
        Standing::InForce
    };

    Assessment {
        id,
        topic: sanitize(&record.topic),
        chosen: record.chosen.as_ref().map(|c| sanitize(&c.approach)),
        alternatives: record
            .alternatives
            .iter()
            .map(|a| sanitize(&a.approach))
            .collect(),
        record,
        assumptions,
        standing,
        fallen,
        stale,
        unchecked,
        reconsider,
        notes,
        superseded_by: None,
    }
}

/// Bewertet alle Records, löst die `supersedes`-Ketten auf und sortiert:
/// Spitzen zuerst — offen, dann neu zu bewerten, dann anzusehen, dann
/// ungeprüft, dann in Kraft; innerhalb dessen die jüngste zuerst —,
/// abgelöste Records danach. Ausgewertet wird jüngste zuerst, damit das
/// geteilte Lauf-Budget denen zugutekommt, die der Reader zeigt — in
/// welcher Reihenfolge der Store liefert, ist egal.
pub fn assess_all(
    records: Vec<(ContentHash, DecisionRecord)>,
    tree: &dyn Worktree,
) -> Vec<Assessment> {
    // Der Zeitstempel einmal geparst; danach jüngste zuerst, Gleichstand
    // nach Id — diese Ordnung trägt alles Weitere.
    let mut records: Vec<(Timestamp, ContentHash, DecisionRecord)> = records
        .into_iter()
        .map(|(id, record)| (at_of(&record), id, record))
        .collect();
    records.sort_by(|(a_at, a_id, _), (b_at, b_id, _)| b_at.cmp(a_at).then(a_id.cmp(b_id)));

    // Nachfolger: Zeigen mehrere Records auf denselben Vorgänger, löst der
    // jüngere ab (Gleichstand: die kleinere Id) — in der Sortierung oben
    // ist das der erste, der den Vorgänger nennt.
    let known: BTreeSet<&ContentHash> = records.iter().map(|(_, id, _)| id).collect();
    let mut successor: BTreeMap<ContentHash, ContentHash> = BTreeMap::new();
    for (_, id, record) in &records {
        let Some(old) = &record.supersedes else {
            continue;
        };
        // Ein Record, der die Prüfung nicht besteht, löst nichts ab: Er
        // ist keine Entscheidung, nur ein Dokument im Store.
        if old == id || !known.contains(old) || record.check().is_err() {
            continue;
        }
        successor.entry(old.clone()).or_insert_with(|| id.clone());
    }
    let cyclic = cycles(&successor);
    let head = |id: &ContentHash| cyclic.contains(id) || !successor.contains_key(id);

    // Die Probe-Plätze bekommen die Spitzen zuerst, dann die jüngsten:
    // Ein alter Head darf nicht hinter jüngeren, abgelösten Records aus
    // dem Lauf fallen.
    records.sort_by(|(a_at, a_id, _), (b_at, b_id, _)| {
        head(b_id)
            .cmp(&head(a_id))
            .then(b_at.cmp(a_at))
            .then(a_id.cmp(b_id))
    });

    // Die Probe-Plätze zählen nur gültige Records: Ungültige bekommen
    // ohnehin keine Probe und sollen keinen Platz wegnehmen.
    let run = Run::new(tree);
    let mut slots = MAX_ASSESSED;
    let mut out: Vec<(Timestamp, Assessment)> = records
        .into_iter()
        .map(|(at, id, record)| {
            let probes = record.check().is_ok() && slots > 0;
            if probes {
                slots -= 1;
            }
            let mut assessment = assess_in(id.clone(), record, &run, probes);
            if !cyclic.contains(&id) {
                assessment.superseded_by = successor.get(&id).cloned();
            }
            (at, assessment)
        })
        .collect();
    out.sort_by(|(a_at, a), (b_at, b)| {
        b.is_head()
            .cmp(&a.is_head())
            .then(a.standing.cmp(&b.standing))
            .then(b_at.cmp(a_at))
            .then(a.id.cmp(&b.id))
    });
    out.into_iter().map(|(_, a)| a).collect()
}

/// Der Zeitstempel zur Sortierung — kein String-Vergleich; unlesbar ist
/// ältestmöglich.
fn at_of(record: &DecisionRecord) -> Timestamp {
    record.at.parse().unwrap_or(Timestamp::MIN)
}

/// Betrifft die Entscheidung diesen Pfad? Ohne Scope: das ganze
/// Repository. Erschöpft ein Glob sein Budget, gilt „betrifft" — die
/// Entscheidung wird lieber einmal zu viel gezeigt als verschwiegen.
pub fn concerns(record: &DecisionRecord, path: &str) -> bool {
    record.scope.is_empty()
        || record.scope.len() > minds_core::decision::MAX_ITEMS
        || record
            .scope
            .iter()
            .any(|glob| !is_glob(glob) || Glob::new(glob).matches_checked(path).unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::decision::{
        Alternative, Assumption, Choice, DECISION_KIND, DECISION_SCHEMA, DecidedBy,
    };

    fn id(n: u8) -> ContentHash {
        ContentHash::from_bytes([n; 32])
    }

    fn reason(assumption: &str, weight: Weight) -> Reason {
        Reason {
            assumption: assumption.into(),
            weight,
        }
    }

    /// flock statt Socket: a1 ohne Probe, a2 Muster in einer Datei, a3
    /// Muster über einen Glob.
    fn sample() -> DecisionRecord {
        DecisionRecord {
            kind: DECISION_KIND.into(),
            schema: DECISION_SCHEMA,
            topic: "lock for the witness socket on macOS".into(),
            decided_by: DecidedBy::Agent,
            at: "2026-10-21T13:09:00Z".into(),
            chosen: Some(Choice {
                approach: "flock on a lock file".into(),
                rests_on: vec![reason("a1", Weight::LoadBearing)],
            }),
            open: false,
            alternatives: vec![
                Alternative {
                    approach: "Unix socket as the lock".into(),
                    rejected_because: vec![
                        reason("a2", Weight::LoadBearing),
                        reason("a3", Weight::Marginal),
                    ],
                    rests_on: Vec::new(),
                },
                Alternative {
                    approach: "named pipe".into(),
                    rejected_because: vec![reason("a3", Weight::LoadBearing)],
                    rests_on: Vec::new(),
                },
            ],
            assumptions: vec![
                Assumption {
                    id: "a1".into(),
                    statement: "the lock file lives on a local file system".into(),
                    probe: None,
                },
                Assumption {
                    id: "a2".into(),
                    statement: "daemon.rs forks".into(),
                    probe: Some(Probe::Pattern {
                        path: Some("src/daemon.rs".into()),
                        glob: None,
                        must_match: Some(r"\bfork\(".into()),
                        must_not_match: None,
                    }),
                },
                Assumption {
                    id: "a3".into(),
                    statement: "no Windows support is needed".into(),
                    probe: Some(Probe::Pattern {
                        path: None,
                        glob: Some("crates/*/Cargo.toml".into()),
                        must_match: None,
                        must_not_match: Some("windows-sys".into()),
                    }),
                },
            ],
            scope: vec!["src/**".into()],
            supersedes: None,
        }
    }

    fn tree_as_decided() -> MemoryTree {
        MemoryTree::new()
            .with("src/daemon.rs", b"fn main() { unsafe { fork() }; }")
            .with("crates/a/Cargo.toml", b"[dependencies]\nserde = \"1\"\n")
    }

    fn pattern(glob: &str, must: Option<&str>, must_not: Option<&str>) -> Probe {
        Probe::Pattern {
            path: None,
            glob: Some(glob.into()),
            must_match: must.map(Into::into),
            must_not_match: must_not.map(Into::into),
        }
    }

    fn in_file(path: &str, must: &str) -> Probe {
        Probe::Pattern {
            path: Some(path.into()),
            glob: None,
            must_match: Some(must.into()),
            must_not_match: None,
        }
    }

    #[test]
    fn as_decided_everything_holds_and_the_decision_is_in_force() {
        let a = assess(id(1), sample(), &tree_as_decided());
        assert_eq!(a.standing, Standing::InForce);
        assert_eq!(
            a.assumptions.iter().map(|s| s.result).collect::<Vec<_>>(),
            vec![
                ProbeResult::Unverifiable,
                ProbeResult::Holds,
                ProbeResult::Holds
            ]
        );
        assert!(a.reconsider.is_empty());
        assert!(a.notes.is_empty());
        assert_eq!(a.headline(), "in force · 2 hold · 1 not checkable");
        assert_eq!(a.topic, "lock for the witness socket on macOS");
        assert_eq!(a.chosen.as_deref(), Some("flock on a lock file"));
    }

    #[test]
    fn a_fallen_load_bearing_rejection_reason_brings_the_alternative_back() {
        // Der Daemon forkt nicht mehr: a2 fällt — und nur der Socket kommt
        // zurück, nicht die named pipe (a3 gilt weiter).
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fn main() { spawn() }")
            .with("crates/a/Cargo.toml", b"[dependencies]\n");
        let a = assess(id(1), sample(), &tree);
        assert_eq!(a.standing, Standing::Review);
        assert!(a.fallen.is_empty(), "der gewählte Weg ruht nicht auf a2");
        assert_eq!(
            a.reconsider,
            vec![Reconsider {
                alternative: "Unix socket as the lock".into(),
                fallen: vec!["a2".into()],
            }]
        );
        assert_eq!(
            a.headline(),
            "re-evaluate: a2 no longer holds → reconsider Unix socket as the lock"
        );
        // Kommen zwei zurück, sagt der Einzeiler das.
        let mut two = sample();
        two.alternatives[1].rejected_because = vec![reason("a2", Weight::LoadBearing)];
        let a = assess(id(1), two, &tree);
        assert_eq!(a.reconsider.len(), 2);
        assert!(
            a.headline()
                .ends_with("reconsider Unix socket as the lock (+1 more)")
        );
    }

    #[test]
    fn a_fallen_marginal_reason_is_only_a_note_and_never_counts_as_holding() {
        // Windows kommt dazu: a3 fällt. Für den Socket war das Randsache
        // (Vermerk), für die named pipe tragend (reconsider).
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fork()")
            .with("crates/a/Cargo.toml", b"windows-sys = \"0.59\"\n");
        let a = assess(id(1), sample(), &tree);
        assert_eq!(a.standing, Standing::Review);
        assert_eq!(a.reconsider.len(), 1);
        assert_eq!(a.reconsider[0].alternative, "named pipe");
        assert_eq!(
            a.notes,
            vec!["a3 no longer holds (marginal) — behind rejecting Unix socket as the lock"]
        );

        // Nur die Randannahme betroffen: Stand bleibt in Kraft — aber die
        // gefallene Annahme zählt nicht als „hold".
        let mut only_marginal = sample();
        only_marginal.alternatives.pop();
        let a = assess(id(1), only_marginal.clone(), &tree);
        assert_eq!(a.standing, Standing::InForce);
        assert_eq!(a.notes.len(), 1);
        assert_eq!(
            a.headline(),
            "in force · 1 holds · 1 fell (noted) · 1 not checkable"
        );

        // Eine veränderte Randannahme wird ebenso vermerkt.
        let mut changed = only_marginal;
        changed.assumptions[2].probe = Some(Probe::Blob {
            path: "crates/a/Cargo.toml".into(),
            blob: "cccc".repeat(10),
        });
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fork()")
            .with_blob("crates/a/Cargo.toml", b"", &"dddd".repeat(10));
        let a = assess(id(1), changed, &tree);
        assert_eq!(a.standing, Standing::InForce);
        assert_eq!(
            a.notes,
            vec![
                "a3 changed since the decision (marginal) — behind rejecting Unix socket as the lock"
            ]
        );
        assert_eq!(
            a.headline(),
            "in force · 1 holds · 1 changed (noted) · 1 not checkable"
        );
    }

    #[test]
    fn a_fallen_assumption_on_an_alternatives_rests_on_is_noted_without_a_weight_claim() {
        // Tragend für das `rests_on` einer Alternative, aber nicht
        // entscheidend: in Kraft, vermerkt, und die Headline schreibt der
        // Annahme kein Gewicht zu.
        let mut record = sample();
        record.alternatives[1].rejected_because.clear();
        record.alternatives[1].rests_on = vec![reason("a3", Weight::LoadBearing)];
        record.alternatives[0].rejected_because.pop();
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fork()")
            .with("crates/a/Cargo.toml", b"windows-sys = \"0.59\"\n");
        let a = assess(id(1), record, &tree);
        assert_eq!(a.standing, Standing::InForce);
        assert_eq!(
            a.notes,
            vec!["a3 no longer holds (load-bearing) — named pipe rests on it"]
        );
        assert_eq!(
            a.headline(),
            "in force · 1 holds · 1 fell (noted) · 1 not checkable"
        );
    }

    #[test]
    fn a_fallen_assumption_of_the_chosen_path_is_the_stronger_signal() {
        let mut record = sample();
        record.assumptions[0].probe = Some(Probe::Path {
            path: "lock".into(),
            exists: true,
        });
        // Nur der gewählte Weg betroffen.
        let a = assess(id(1), record.clone(), &tree_as_decided());
        assert_eq!(a.standing, Standing::Review);
        assert_eq!(a.fallen, vec!["a1"]);
        assert!(a.reconsider.is_empty());
        assert_eq!(
            a.headline(),
            "re-evaluate: a1 no longer holds (the chosen path rests on it)"
        );
        // Beides belegt: Die Headline nennt den gewählten Weg, nicht die
        // Alternative.
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"spawn()")
            .with("crates/a/Cargo.toml", b"");
        let a = assess(id(1), record, &tree);
        assert_eq!(a.fallen, vec!["a1"]);
        assert_eq!(a.reconsider.len(), 1);
        assert!(a.headline().contains("the chosen path rests on it"));
    }

    #[test]
    fn a_changed_blob_is_re_check_not_review() {
        let mut record = sample();
        record.assumptions[1].probe = Some(Probe::Blob {
            path: "src/daemon.rs".into(),
            blob: "aaaa".repeat(10),
        });
        let same = MemoryTree::new()
            .with_blob("src/daemon.rs", b"x", &"aaaa".repeat(10))
            .with("crates/a/Cargo.toml", b"");
        assert_eq!(
            assess(id(1), record.clone(), &same).standing,
            Standing::InForce
        );
        let changed = MemoryTree::new()
            .with_blob("src/daemon.rs", b"y", &"bbbb".repeat(10))
            .with("crates/a/Cargo.toml", b"");
        let a = assess(id(1), record.clone(), &changed);
        assert_eq!(a.standing, Standing::ReCheck);
        assert_eq!(a.stale, vec!["a2"]);
        assert_eq!(a.headline(), "re-check: a2 changed since the decision");
        // Eine gelöschte Datei ist ebenfalls eine Änderung …
        let gone = MemoryTree::new().with("crates/a/Cargo.toml", b"");
        assert_eq!(
            assess(id(1), record.clone(), &gone).standing,
            Standing::ReCheck
        );
        // … eine unlesbare nicht: Das ist nicht entscheidbar.
        let unreadable = MemoryTree::new()
            .with_unreadable("src/daemon.rs")
            .with("crates/a/Cargo.toml", b"");
        let a = assess(id(1), record, &unreadable);
        assert_eq!(
            a.assumptions[1].result,
            ProbeResult::Unknown("file not readable")
        );
        assert_eq!(a.standing, Standing::Unchecked);
    }

    #[test]
    fn an_open_decision_stays_open_whatever_the_probes_say() {
        let mut record = sample();
        record.chosen = None;
        record.open = true;
        for alternative in &mut record.alternatives {
            alternative.rests_on = std::mem::take(&mut alternative.rejected_because);
        }
        let tree = MemoryTree::new().with("src/daemon.rs", b"spawn()");
        let a = assess(id(1), record, &tree);
        assert_eq!(a.standing, Standing::Open);
        assert_eq!(a.headline(), "open — waiting for a human · 2 options");
        assert_eq!(a.chosen, None);
        // Die Proben sind trotzdem ausgewertet — der Mensch sieht den
        // Stand, und das `rests_on` einer Option wird vermerkt.
        assert_eq!(a.assumptions[1].result, ProbeResult::Fails);
        assert!(a.reconsider.is_empty());
        assert_eq!(
            a.notes[0],
            "a2 no longer holds (load-bearing) — Unix socket as the lock rests on it"
        );
    }

    #[test]
    fn a_load_bearing_probe_that_cannot_run_makes_the_decision_unchecked() {
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fork()")
            .with_unreadable("crates/a/Cargo.toml");
        let a = assess(id(1), sample(), &tree);
        assert_eq!(
            a.assumptions[2].result,
            ProbeResult::Unknown("file not readable")
        );
        // a3 ist tragend für die named pipe → nicht „in Kraft".
        assert_eq!(a.standing, Standing::Unchecked);
        assert_eq!(a.unchecked, vec!["a3"]);
        assert_eq!(a.headline(), "unchecked: a3 could not be probed");
        assert!(a.notes.iter().any(|n| n
            == "a3 not checked (file not readable, marginal) — behind rejecting Unix socket as the lock"));
        assert!(
            a.notes
                .iter()
                .any(|n| n == "a3 not checked (file not readable) — behind rejecting named pipe")
        );
    }

    #[test]
    fn a_record_that_fails_the_check_gets_no_probe_even_when_it_says_open() {
        // Ein Record am Binary vorbei geschrieben: Pfad außerhalb des
        // Repositories. Keine Probe läuft, der Stand ist `unchecked`.
        let mut record = sample();
        record.assumptions[1].probe = Some(in_file("../../etc/passwd", "root"));
        let tree = MemoryTree::new().with("../../etc/passwd", b"root:x");
        let a = assess(id(1), record.clone(), &tree);
        assert_eq!(a.assumptions[0].result, ProbeResult::Unverifiable);
        assert_eq!(
            a.assumptions[1].result,
            ProbeResult::Unknown("record invalid")
        );
        assert_eq!(
            a.assumptions[2].result,
            ProbeResult::Unknown("record invalid")
        );
        assert_eq!(a.standing, Standing::Unchecked);
        assert_eq!(
            a.headline(),
            "unchecked: the record does not pass the check"
        );
        assert!(
            a.notes[0].starts_with("record does not pass the check: probe of assumption \"a2\"")
        );
        // Auch `open` hebt ihn nicht an die Spitze.
        let mut open = record;
        open.open = true; // offen *und* gewählt: besteht die Prüfung nicht
        let a = assess(id(1), open, &tree);
        assert_eq!(a.standing, Standing::Unchecked);
        assert_ne!(a.headline(), "open — waiting for a human · 2 options");
        // Und `evaluate` allein lässt einen solchen Pfad ebenfalls nicht an
        // den Worktree.
        assert_eq!(
            evaluate(Some(&in_file("../../etc/passwd", "root")), &tree),
            ProbeResult::Unknown("path is not repo-relative")
        );
        assert_eq!(
            evaluate(
                Some(&Probe::Path {
                    path: "C:/Users/x".into(),
                    exists: true
                }),
                &tree
            ),
            ProbeResult::Unknown("path is not repo-relative")
        );
    }

    #[test]
    fn a_dot_segment_is_not_the_listed_spelling() {
        // `./src/lib.rs` ist kein kanonischer Pfad: check() lehnt ihn ab,
        // und evaluate() lässt ihn nicht an die Dateiliste.
        let tree = MemoryTree::new().with("src/lib.rs", b"");
        let probe = Probe::Path {
            path: "./src/lib.rs".into(),
            exists: true,
        };
        assert_eq!(
            evaluate(Some(&probe), &tree),
            ProbeResult::Unknown("path is not repo-relative")
        );
    }

    #[test]
    fn an_invalid_record_supersedes_nothing() {
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"fork()")
            .with("crates/a/Cargo.toml", b"");
        let base = sample();
        let mut bogus = sample();
        bogus.supersedes = Some(id(1));
        bogus.at = "2026-10-23T00:00:00Z".into();
        bogus.kind = "note".into(); // besteht die Prüfung nicht
        let all = assess_all(vec![(id(1), base), (id(2), bogus)], &tree);
        let base = all.iter().find(|a| a.id == id(1)).unwrap();
        assert!(base.is_head());
        assert_eq!(base.standing, Standing::InForce);
        let bogus = all.iter().find(|a| a.id == id(2)).unwrap();
        assert_eq!(bogus.standing, Standing::Unchecked);
    }

    #[test]
    fn an_unreferenced_assumption_that_fell_is_noted_so_the_count_is_explained() {
        let mut record = sample();
        record.alternatives.clear();
        record.chosen.as_mut().unwrap().rests_on.clear();
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"spawn()")
            .with("crates/a/Cargo.toml", b"");
        let a = assess(id(1), record, &tree);
        assert_eq!(a.standing, Standing::InForce);
        assert_eq!(
            a.headline(),
            "in force · 1 holds · 1 fell (noted) · 1 not checkable"
        );
        assert_eq!(a.notes, vec!["a2 no longer holds — not tied to any path"]);
    }

    #[test]
    fn a_file_name_longer_than_a_path_may_be_makes_a_glob_probe_undecidable() {
        let tree = MemoryTree::new()
            .with("a.rs", b"")
            .with(&format!("{}.rs", "b".repeat(MAX_PATH)), b"x");
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("x"))), &tree),
            ProbeResult::Unknown("a file name is not listable")
        );
    }

    #[test]
    fn only_listed_files_are_ever_read() {
        // `.env` liegt auf der Platte, steht aber nicht in der Dateiliste
        // (ignoriert). Keine Probe darf ihren Inhalt oder ihre Existenz
        // verraten.
        let tree = MemoryTree::new()
            .with("src/lib.rs", b"")
            .with_hidden(".env", b"OPENAI_API_KEY=sk-abc");
        assert_eq!(
            evaluate(Some(&in_file(".env", "sk-a")), &tree),
            ProbeResult::Unknown("path not in the repository")
        );
        assert_eq!(
            evaluate(Some(&in_file(".env", "sk-z")), &tree),
            ProbeResult::Unknown("path not in the repository")
        );
        let exists = Probe::Path {
            path: ".env".into(),
            exists: true,
        };
        assert_eq!(evaluate(Some(&exists), &tree), ProbeResult::Fails);
        let absent = Probe::Path {
            path: ".env".into(),
            exists: false,
        };
        assert_eq!(evaluate(Some(&absent), &tree), ProbeResult::Holds);
        // Ein Glob sieht nur die Liste.
        assert_eq!(
            evaluate(Some(&pattern("*", Some("sk-"), None)), &tree),
            ProbeResult::Fails
        );
        // Blob: nicht gelistet heißt nicht mehr da.
        let blob = Probe::Blob {
            path: ".env".into(),
            blob: "a".repeat(40),
        };
        assert_eq!(evaluate(Some(&blob), &tree), ProbeResult::Stale);
    }

    #[test]
    fn probes_that_cannot_decide_say_why_and_never_count_as_holding() {
        let x = MemoryTree::new().with("x", b"");
        assert_eq!(
            evaluate(Some(&in_file("x", "(")), &x),
            ProbeResult::Unknown("pattern invalid or too large")
        );
        let big = MemoryTree::new().with("x", &vec![b'a'; MAX_FILE_BYTES + 1]);
        assert_eq!(
            evaluate(Some(&in_file("x", "a")), &big),
            ProbeResult::Unknown("file too large")
        );
        // Eine zu große Datei vor dem Treffer: unentscheidbar (die Liste
        // ist sortiert, `a.rs` kommt vor `b.rs`).
        let mixed = MemoryTree::new()
            .with("a.rs", &vec![b'a'; MAX_FILE_BYTES + 1])
            .with("b.rs", b"hit");
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("hit"), None)), &mixed),
            ProbeResult::Unknown("file too large")
        );
        // Eine Dateiliste, die nicht kommt, bestätigt kein „kommt nicht vor".
        let unlisted = MemoryTree::new().without_listing();
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("x"))), &unlisted),
            ProbeResult::Unknown("files not listable")
        );
        // Ein Dateiname, der die Regeln nicht besteht, macht die Glob-Probe
        // unentscheidbar — er könnte das Muster enthalten.
        let odd = MemoryTree::new()
            .with("a.rs", b"")
            .with("we\u{7}ird.rs", b"x");
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("x"))), &odd),
            ProbeResult::Unknown("a file name is not listable")
        );
        // Zu viele Dateien.
        let mut many = MemoryTree::new();
        for i in 0..=MAX_GLOB_FILES {
            many = many.with(&format!("f{i}.rs"), b"");
        }
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("x"))), &many),
            ProbeResult::Unknown("too many files")
        );
        // Beide Pfadangaben zugleich.
        let both = Probe::Pattern {
            path: Some("a".into()),
            glob: Some("*".into()),
            must_match: Some("x".into()),
            must_not_match: None,
        };
        assert_eq!(
            evaluate(Some(&both), &MemoryTree::new()),
            ProbeResult::Unknown("probe has both path and glob")
        );
    }

    #[test]
    fn a_worktree_that_returns_more_than_asked_is_not_trusted() {
        struct Greedy;
        impl Worktree for Greedy {
            fn files(&self) -> Result<Vec<String>, ReadError> {
                Ok(vec!["x".into()])
            }
            fn read(&self, _: &str, max: usize) -> Result<Vec<u8>, ReadError> {
                Ok(vec![b'a'; max + 1])
            }
            fn blob_id(&self, _: &str) -> Result<String, ReadError> {
                Err(ReadError::Missing)
            }
        }
        assert_eq!(
            evaluate(Some(&in_file("x", "a")), &Greedy),
            ProbeResult::Unknown("worktree returned more than asked")
        );
    }

    #[test]
    fn a_pattern_that_needs_kilobytes_is_refused_and_the_run_has_a_deadline() {
        // Ein Muster, das den Matcher beschäftigen soll, kompiliert unter
        // der Größenbeschränkung nicht.
        let tree = MemoryTree::new().with("x", b"abab");
        assert_eq!(
            evaluate(Some(&in_file("x", "[ab]*a[ab]{3000}c")), &tree),
            ProbeResult::Unknown("pattern invalid or too large")
        );
        assert_eq!(
            evaluate(Some(&in_file("x", "[ab]*a[ab]{100}c")), &tree),
            ProbeResult::Unknown("pattern invalid or too large")
        );
        for huge in [r"(?u)\p{L}{2000}", "((a{100}){100}){100}"] {
            assert_eq!(
                evaluate(Some(&in_file("x", huge)), &tree),
                ProbeResult::Unknown("pattern invalid or too large")
            );
        }
        // Die Muster, um die es geht, passen — auch mit Klassen.
        assert_eq!(
            evaluate(Some(&in_file("x", r"\bab(ab)+\b")), &tree),
            ProbeResult::Holds
        );
        let code =
            MemoryTree::new().with("y", b"fn main() -> Result<(), E> {}\n// TODO 2026-10-21");
        for pattern in [
            r"fn\s+\w+\s*\([^)]*\)\s*->\s*Result<",
            r"\b(TODO|FIXME|XXX)\b.*\d{4}-\d{2}-\d{2}",
            r"(?m)^\s*fn main",
        ] {
            assert_eq!(
                evaluate(Some(&in_file("y", pattern)), &code),
                ProbeResult::Holds,
                "{pattern}"
            );
        }
        // Nach der Frist läuft keine Probe mehr.
        let run = Run::with_deadline(&tree, Instant::now());
        assert_eq!(
            evaluate_in(Some(&in_file("x", "a")), &run, &Budget::record()),
            ProbeResult::Unknown("time budget exhausted")
        );
    }

    #[test]
    fn pattern_probe_semantics() {
        let tree = MemoryTree::new()
            .with("a.rs", b"alpha")
            .with("b.rs", b"beta")
            .with("c.txt", b"gamma");
        // must_match: in mindestens einer Datei des Globs.
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("beta"), None)), &tree),
            ProbeResult::Holds
        );
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("gamma"), None)), &tree),
            ProbeResult::Fails
        );
        // must_not_match: in keiner.
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("gamma"))), &tree),
            ProbeResult::Holds
        );
        assert_eq!(
            evaluate(Some(&pattern("*.rs", None, Some("alpha"))), &tree),
            ProbeResult::Fails
        );
        // Beides.
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("alpha"), Some("gamma"))), &tree),
            ProbeResult::Holds
        );
        // Ein Glob ohne Treffer: Was vorkommen muss, kommt nicht vor; was
        // nicht vorkommen darf, ist ohne Dateien nicht entscheidbar.
        assert_eq!(
            evaluate(Some(&pattern("*.py", Some("x"), None)), &tree),
            ProbeResult::Fails
        );
        assert_eq!(
            evaluate(Some(&pattern("*.py", None, Some("x"))), &tree),
            ProbeResult::Unknown("glob matches no file")
        );
        // Ein Dateiname mit `\` oder `:` ist in der Liste kein Problem —
        // außer unter Windows, wo er ein Laufwerk oder Stream wäre.
        let odd = MemoryTree::new()
            .with("fixtures/win\\path.txt", b"alpha")
            .with("a:b.rs", b"beta");
        assert_eq!(
            evaluate(Some(&pattern("**", Some("beta"), None)), &odd),
            if cfg!(windows) {
                ProbeResult::Unknown("a file name is not listable")
            } else {
                ProbeResult::Holds
            }
        );
        // `.git`-Aliasse in der Liste machen jede Probe unentscheidbar.
        for name in ["GIT~1/config", ".git./config", "a/.git /x", "sub/.GIT/x"] {
            let aliased = MemoryTree::new().with("a.rs", b"").with(name, b"");
            assert_eq!(
                evaluate(Some(&pattern("*.rs", Some("x"), None)), &aliased),
                ProbeResult::Unknown("a file name is not listable"),
                "{name}"
            );
        }
        // Ein Glob, der das Repository verlässt, läuft nicht.
        assert_eq!(
            evaluate(Some(&pattern("../**", Some("x"), None)), &tree),
            ProbeResult::Unknown("glob is not repo-relative")
        );
        // Muster laufen über Bytes — eine Binärdatei bricht nichts.
        let binary = MemoryTree::new().with("bin", &[0xff, 0xfe, b'o', b'k', 0x00]);
        assert_eq!(
            evaluate(Some(&in_file("bin", "ok")), &binary),
            ProbeResult::Holds
        );
    }

    #[test]
    fn path_probe_semantics() {
        let tree = MemoryTree::new().with("src/lib.rs", b"");
        let exists = |path: &str, exists: bool| Probe::Path {
            path: path.into(),
            exists,
        };
        assert_eq!(
            evaluate(Some(&exists("src/lib.rs", true)), &tree),
            ProbeResult::Holds
        );
        assert_eq!(
            evaluate(Some(&exists("src", true)), &tree),
            ProbeResult::Holds
        );
        assert_eq!(
            evaluate(Some(&exists("src/main.rs", true)), &tree),
            ProbeResult::Fails
        );
        assert_eq!(
            evaluate(Some(&exists("src/main.rs", false)), &tree),
            ProbeResult::Holds
        );
        assert_eq!(
            evaluate(Some(&exists("sr", true)), &tree),
            ProbeResult::Fails
        );
        assert_eq!(evaluate(None, &tree), ProbeResult::Unverifiable);
    }

    #[test]
    fn chains_are_resolved_and_heads_sorted_by_urgency_then_age() {
        let tree = MemoryTree::new()
            .with("src/daemon.rs", b"spawn()")
            .with("crates/a/Cargo.toml", b"");
        let mut old = sample();
        old.at = "2026-09-01T00:00:00Z".into();
        let mut new = sample();
        new.supersedes = Some(id(1));
        new.assumptions[1].probe = None; // kein fork mehr nötig
        let mut open = sample();
        open.chosen = None;
        open.open = true;
        open.topic = "retry policy".into();
        open.at = "2026-10-01T00:00:00Z".into();
        for alternative in &mut open.alternatives {
            alternative.rejected_because.clear();
        }
        let mut fine = sample();
        fine.topic = "something in force".into();
        fine.at = "2026-10-22T00:00:00Z".into(); // jünger als `new`
        fine.assumptions.clear();
        fine.alternatives.clear();
        fine.chosen.as_mut().unwrap().rests_on.clear();

        let all = assess_all(
            vec![(id(1), old), (id(2), new), (id(3), open), (id(4), fine)],
            &tree,
        );
        let order: Vec<(u8, Standing, bool)> = all
            .iter()
            .map(|a| (a.id.to_bytes()[0], a.standing, a.is_head()))
            .collect();
        assert_eq!(
            order,
            vec![
                (3, Standing::Open, true),
                // Beide in Kraft: die jüngere zuerst, obwohl ihre Id größer ist.
                (4, Standing::InForce, true),
                (2, Standing::InForce, true),
                // Der abgelöste Record: bewertet, aber nach den Spitzen.
                (1, Standing::Review, false),
            ]
        );
        assert_eq!(all[3].superseded_by, Some(id(2)));
        // Ein `supersedes` auf einen unbekannten Record macht keinen
        // Nachfolger — und einen Record nicht zur Nicht-Spitze.
        let mut stray = sample();
        stray.supersedes = Some(id(9));
        let all = assess_all(vec![(id(5), stray)], &tree);
        assert!(all[0].is_head());
    }

    #[test]
    fn two_successors_the_younger_wins_in_any_input_order() {
        let tree = MemoryTree::new();
        let base = sample();
        let mut older = sample();
        older.supersedes = Some(id(1));
        older.at = "2026-10-22T00:00:00Z".into();
        let mut younger = sample();
        younger.supersedes = Some(id(1));
        younger.at = "2026-10-23T00:00:00Z".into();
        for order in [[2u8, 3], [3, 2]] {
            let records = order
                .iter()
                .map(|n| {
                    (
                        id(*n),
                        if *n == 2 {
                            older.clone()
                        } else {
                            younger.clone()
                        },
                    )
                })
                .chain(std::iter::once((id(1), base.clone())))
                .collect();
            let all = assess_all(records, &tree);
            let base = all.iter().find(|a| a.id == id(1)).unwrap();
            assert_eq!(base.superseded_by, Some(id(3)), "{order:?}");
            assert!(all.iter().filter(|a| a.is_head()).count() == 2);
        }
    }

    #[test]
    fn long_chains_resolve_in_linear_time_and_only_the_youngest_are_probed() {
        // 20 000 Records in einer Kette: Zyklen-Suche und Auflösung bleiben
        // weit unter einer Sekunde; Proben laufen nur für die jüngsten.
        let tree = MemoryTree::new().with("src/daemon.rs", b"fork()");
        let mut base = sample();
        base.assumptions.truncate(2);
        base.alternatives.clear();
        base.chosen.as_mut().unwrap().rests_on = vec![reason("a2", Weight::LoadBearing)];
        let n: u32 = 20_000;
        let id_of = |i: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&i.to_be_bytes());
            ContentHash::from_bytes(bytes)
        };
        let records: Vec<(ContentHash, DecisionRecord)> = (0..n)
            .map(|i| {
                let mut r = base.clone();
                r.at = "2026-01-01T00:00:00Z".to_owned();
                r.topic = format!("t{i}");
                if i > 0 {
                    r.supersedes = Some(id_of(i - 1));
                }
                (id_of(i), r)
            })
            .collect();
        let started = Instant::now();
        let all = assess_all(records, &tree);
        // Großzügig gegen parallele Suiten; quadratisch wären es Minuten.
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(all.iter().filter(|a| a.is_head()).count(), 1);
        assert_eq!(all[0].record.topic, format!("t{}", n - 1));
        let probed = all
            .iter()
            .filter(|a| a.assumptions[1].result == ProbeResult::Holds)
            .count();
        assert_eq!(probed, MAX_ASSESSED);
        let skipped = all
            .iter()
            .filter(|a| {
                a.assumptions[1].result == ProbeResult::Unknown("too many records in this run")
            })
            .count();
        assert_eq!(skipped, n as usize - MAX_ASSESSED);
        assert!(
            all.iter().all(|a| a.standing != Standing::InForce
                || a.assumptions[1].result == ProbeResult::Holds)
        );
    }

    #[test]
    fn a_skipped_record_resting_only_on_unprobed_assumptions_stays_in_force() {
        // Jenseits der Probe-Plätze: Eine Annahme ohne Probe bleibt „nicht
        // prüfbar", nie „nicht geprüft" — der Stand ändert sich nicht.
        let tree = MemoryTree::new();
        let mut base = sample();
        base.assumptions.truncate(1);
        base.alternatives.clear();
        let mut records: Vec<(ContentHash, DecisionRecord)> = (0..=MAX_ASSESSED as u32)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[..4].copy_from_slice(&i.to_be_bytes());
                let mut r = base.clone();
                r.topic = format!("t{i}");
                r.at = format!("2026-01-{:02}T00:00:00Z", 1 + i % 28);
                (ContentHash::from_bytes(bytes), r)
            })
            .collect();
        records.reverse();
        let all = assess_all(records, &tree);
        assert!(
            all.iter().all(|a| a.standing == Standing::InForce),
            "{:?}",
            all[0].standing
        );
        assert!(
            all.iter()
                .all(|a| a.assumptions[0].result == ProbeResult::Unverifiable)
        );
    }

    #[test]
    fn a_listed_file_that_is_missing_is_not_an_empty_file() {
        struct Gone;
        impl Worktree for Gone {
            fn files(&self) -> Result<Vec<String>, ReadError> {
                Ok(vec!["Cargo.toml".into()])
            }
            fn read(&self, _: &str, _: usize) -> Result<Vec<u8>, ReadError> {
                Err(ReadError::Missing)
            }
            fn blob_id(&self, _: &str) -> Result<String, ReadError> {
                Err(ReadError::Missing)
            }
        }
        let probe = Probe::Pattern {
            path: Some("Cargo.toml".into()),
            glob: None,
            must_match: None,
            must_not_match: Some("windows-sys".into()),
        };
        assert_eq!(
            evaluate(Some(&probe), &Gone),
            ProbeResult::Unknown("file missing from the worktree")
        );
        assert_eq!(
            evaluate(Some(&pattern("*.toml", None, Some("windows-sys"))), &Gone),
            ProbeResult::Unknown("file missing from the worktree")
        );
    }

    #[test]
    fn an_unlistable_name_blocks_only_the_probes_it_could_answer() {
        let tree = MemoryTree::new()
            .with("src/lib.rs", b"")
            .with("src/sub/we\u{7}ird.rs", b"");
        let exists = |path: &str, exists: bool| Probe::Path {
            path: path.into(),
            exists,
        };
        // Unter `src/sub` liegt nur der unlistbare Name: nicht entscheidbar.
        assert_eq!(
            evaluate(Some(&exists("src/sub", true)), &tree),
            ProbeResult::Unknown("files not listable")
        );
        assert_eq!(
            evaluate(Some(&exists("src/sub", false)), &tree),
            ProbeResult::Unknown("files not listable")
        );
        // `src` hat einen listbaren Treffer: entschieden.
        assert_eq!(
            evaluate(Some(&exists("src", true)), &tree),
            ProbeResult::Holds
        );
        // Ein anderer Pfad ist davon nicht berührt …
        assert_eq!(
            evaluate(Some(&exists("docs", false)), &tree),
            ProbeResult::Holds
        );
        // … ebenso wenig ein exakter Pfad (der nie unlistbar sein kann).
        let blob = Probe::Blob {
            path: "src/lib.rs".into(),
            blob: "a".repeat(40),
        };
        assert_eq!(
            evaluate(Some(&blob), &tree),
            ProbeResult::Unknown("file not readable")
        );
        assert_eq!(
            evaluate(Some(&in_file("src/lib.rs", "x")), &tree),
            ProbeResult::Fails
        );
        // Ein Glob dagegen immer.
        assert_eq!(
            evaluate(Some(&pattern("docs/**", None, Some("x"))), &tree),
            ProbeResult::Unknown("a file name is not listable")
        );
    }

    #[test]
    fn a_tail_into_a_cycle_stays_a_head_and_both_patterns_without_hits_fail() {
        let tree = MemoryTree::new().with("a.rs", b"");
        let mut x = sample();
        x.supersedes = Some(id(1)); // X ⇒ A
        let mut a = sample();
        a.supersedes = Some(id(2)); // A ⇒ B
        let mut b = sample();
        b.supersedes = Some(id(1)); // B ⇒ A
        let all = assess_all(vec![(id(3), x), (id(1), a), (id(2), b)], &tree);
        assert_eq!(all.iter().filter(|h| h.is_head()).count(), 3);
        // Ohne Treffer: `must_match` entscheidet (nichts kommt vor), auch
        // wenn `must_not_match` dabei ist.
        assert_eq!(
            evaluate(Some(&pattern("*.py", Some("x"), Some("y"))), &tree),
            ProbeResult::Fails
        );
    }

    #[test]
    fn a_must_match_hit_is_enough_and_a_truncating_worktree_is_caught() {
        // Der erste Treffer beweist „kommt vor" — eine zu große Datei
        // dahinter spielt keine Rolle mehr.
        let mixed = MemoryTree::new()
            .with("a.rs", b"hit")
            .with("b.rs", &vec![b'a'; MAX_FILE_BYTES + 1]);
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("hit"), None)), &mixed),
            ProbeResult::Holds
        );
        // Mit `must_not` muss jede Datei gelesen werden.
        assert_eq!(
            evaluate(Some(&pattern("*.rs", Some("hit"), Some("zzz"))), &mixed),
            ProbeResult::Unknown("file too large")
        );
        // Ein Worktree, der statt `TooLarge` still kürzt, fällt auf.
        struct Truncating;
        impl Worktree for Truncating {
            fn files(&self) -> Result<Vec<String>, ReadError> {
                Ok(vec!["x".into()])
            }
            fn read(&self, _: &str, max: usize) -> Result<Vec<u8>, ReadError> {
                Ok(vec![b'a'; max])
            }
            fn blob_id(&self, _: &str) -> Result<String, ReadError> {
                Err(ReadError::Missing)
            }
        }
        assert_eq!(
            evaluate(Some(&in_file("x", "zzz")), &Truncating),
            ProbeResult::Unknown("file too large")
        );
    }

    #[test]
    fn an_old_head_keeps_its_probe_slot_ahead_of_younger_superseded_records() {
        // Ein Head von 2020 hinter 512 jüngeren, abgelösten Records: Er
        // bekommt trotzdem seine Probe.
        let tree = MemoryTree::new().with("src/daemon.rs", b"fork()");
        let mut base = sample();
        base.assumptions.truncate(2);
        base.alternatives.clear();
        base.chosen.as_mut().unwrap().rests_on = vec![reason("a2", Weight::LoadBearing)];
        let id_of = |i: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&i.to_be_bytes());
            ContentHash::from_bytes(bytes)
        };
        let mut records: Vec<(ContentHash, DecisionRecord)> = Vec::new();
        let mut old_head = base.clone();
        old_head.at = "2020-01-01T00:00:00Z".into();
        old_head.topic = "old head".into();
        records.push((id_of(0), old_head));
        // Eine Kette aus jüngeren Records; alle außer dem letzten abgelöst.
        let n = MAX_ASSESSED as u32 + 8;
        for i in 1..=n {
            let mut r = base.clone();
            r.at = "2026-01-01T00:00:00Z".into();
            r.topic = format!("t{i}");
            if i > 1 {
                r.supersedes = Some(id_of(i - 1));
            }
            records.push((id_of(i), r));
        }
        let all = assess_all(records, &tree);
        let old = all.iter().find(|a| a.id == id_of(0)).unwrap();
        assert!(old.is_head());
        assert_eq!(old.assumptions[1].result, ProbeResult::Holds);
        assert_eq!(old.standing, Standing::InForce);
    }

    #[test]
    fn a_cycle_has_no_head_and_stays_visible() {
        let tree = MemoryTree::new();
        let mut a = sample();
        a.supersedes = Some(id(2));
        let mut b = sample();
        b.supersedes = Some(id(1));
        b.at = "2026-10-22T00:00:00Z".into();
        let all = assess_all(vec![(id(1), a), (id(2), b)], &tree);
        assert!(all.iter().all(|x| x.is_head()));
        // Ein Record, der sich selbst ablöst, ebenso.
        let mut selfish = sample();
        selfish.supersedes = Some(id(7));
        let all = assess_all(vec![(id(7), selfish)], &tree);
        assert!(all[0].is_head());
    }

    #[test]
    fn scope_decides_what_a_path_concerns() {
        let record = sample();
        assert!(concerns(&record, "src/daemon.rs"));
        assert!(!concerns(&record, "docs/x.md"));
        let mut whole = sample();
        whole.scope.clear();
        assert!(concerns(&whole, "docs/x.md"));
    }

    #[test]
    fn foreign_text_is_sanitized_in_the_assessment() {
        let mut record = sample();
        record.topic = "lock\u{1b}[2J".into();
        record.chosen.as_mut().unwrap().approach = "flock\u{2028}".into();
        record.assumptions[0].statement = "local\u{1b}[2J fs".into();
        record.alternatives[0].approach = "sock\u{202e}et".into();
        let tree = MemoryTree::new().with("src/daemon.rs", b"spawn()");
        let a = assess(id(1), record, &tree);
        assert!(!a.topic.contains('\u{1b}'));
        assert!(!a.chosen.as_ref().unwrap().contains('\u{2028}'));
        assert!(!a.assumptions[0].statement.contains('\u{1b}'));
        assert!(!a.reconsider[0].alternative.contains('\u{202e}'));
    }

    #[test]
    fn budgets_are_per_probe_per_record_and_per_run_youngest_first() {
        let chunk = vec![b'a'; MAX_FILE_BYTES];
        let per_probe = (MAX_PROBE_BYTES / MAX_FILE_BYTES as u64) as usize;
        let per_run = (MAX_RUN_BYTES / MAX_FILE_BYTES as u64) as usize;
        let mut tree = MemoryTree::new();
        for i in 0..=per_run {
            tree = tree.with(&format!("f{i}.bin"), &chunk);
        }
        // Je Probe: ein Glob über mehr Dateien, als eine Probe lesen darf.
        let glob = pattern("f*.bin", Some("zzz"), None);
        assert!(per_probe < per_run);
        assert_eq!(
            evaluate(Some(&glob), &tree),
            ProbeResult::Unknown("byte budget exhausted")
        );

        // Je Lauf, jüngste zuerst: Records mit je einer Datei; das
        // Lauf-Budget reicht für `per_run` davon — der älteste geht leer
        // aus, egal in welcher Reihenfolge sie kommen.
        let mut base = sample();
        base.assumptions.truncate(1);
        base.alternatives.clear();
        base.chosen.as_mut().unwrap().rests_on.clear();
        let mut records = Vec::new();
        for i in 0..=per_run {
            let mut r = base.clone();
            r.topic = format!("t{i}");
            r.at = format!("2026-10-{:02}T00:00:00Z", 1 + i % 28);
            r.assumptions[0].probe = Some(in_file(&format!("f{i}.bin"), "a"));
            records.push((ContentHash::from_bytes([i as u8; 32]), r));
        }
        records.reverse();
        let all = assess_all(records, &tree);
        let holds = all
            .iter()
            .filter(|a| a.assumptions[0].result == ProbeResult::Holds)
            .count();
        assert_eq!(holds, per_run);
        let starved: Vec<&Assessment> = all
            .iter()
            .filter(|a| a.assumptions[0].result == ProbeResult::Unknown("byte budget exhausted"))
            .collect();
        assert_eq!(starved.len(), 1);
        // Der Älteste (t0, 2026-10-01) ging leer aus.
        assert_eq!(starved[0].record.topic, "t0");
    }

    #[test]
    fn a_glob_budget_is_shared_and_runs_out_with_a_reason() {
        // Ein Glob, der an jedem Pfad teuer ist, erschöpft das Budget des
        // Records — die Probe sagt das, statt „trifft nicht".
        let mut tree = MemoryTree::new();
        for i in 0..64 {
            tree = tree.with(&format!("{}{i:04}", "a".repeat(MAX_PATH - 4)), b"");
        }
        let hostile = pattern(&format!("*{}b", "a".repeat(500)), None, Some("x"));
        assert_eq!(
            evaluate(Some(&hostile), &tree),
            ProbeResult::Unknown("glob budget exhausted")
        );
    }
}
