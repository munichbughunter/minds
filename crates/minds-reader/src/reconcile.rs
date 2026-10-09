//! Reconciliation of committed bytes with stored write evidence. Pure and
//! read-only: callers supply blobs and observations from the session window.
//!
//! Paths are repo-relative identities, not display strings; consumers must
//! sanitize them before rendering. Blob contents never appear in the result.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use minds_core::{ContentHash, Effect, EffectKind, Session, SessionId, ToolCall};
use minds_git::{CommitId, added_line_ranges, removes_lines};

/// Maximum size of any blob or reconstructed content used for line attribution.
pub const LINE_LEVEL_LIMIT: usize = 2 * 1024 * 1024;

/// A commit's derivation, sorted by repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub commit: CommitId,
    /// First parent, or `None` for a root commit.
    pub base: Option<CommitId>,
    pub files: Vec<FileRecon>,
    /// Only witness-backed lines (`Explained`, `ExplainedFsOnly`), never
    /// `ReportedOnly`. For "every line with some evidence" see
    /// [`Reconciliation::backed_lines`].
    pub explained_lines: u64,
    pub total_changed_lines: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecon {
    pub path: String,
    pub class: ReconClass,
    pub line_level: LineLevel,
    /// Blake3 of the committed blob. For deletions, the hash of empty bytes;
    /// `deleted` distinguishes absence from a committed empty file.
    pub committed: ContentHash,
    pub deleted: bool,
    /// Added/modified lines of this file against the base, counted like
    /// `Reconciliation::total_changed_lines` (binary files count one).
    pub changed_lines: u64,
    /// Whether content went missing that the added lines do not show: a
    /// deletion; a binary or oversized modification (unknown amounts); or,
    /// for text, a diff hunk that removes more lines than it adds — against
    /// the base, or against the claimed content (written by the agent, then
    /// removed). A replacement in place shows as added lines and does not
    /// count.
    pub removes: bool,
    /// Last observation matching the committed state, even if a later one
    /// contradicts it. Classification always uses the latest observation.
    pub last_observed: Option<ObservedAt>,
    /// Warum die Datei unerklärt ist — `Some` genau bei
    /// [`ReconClass::Unexplained`]. Gilt für alle ihre unerklärten Zeilen.
    pub gap: Option<Gap>,
}

/// Warum geänderte Zeilen ohne Beleg sind — aus dem, was die Sessions über
/// die Datei wissen. Eine Ableitung für die Anzeige; Klasse, Anteil und Gate
/// hängen nie daran. Jede Variante sagt nur, was die gespeicherten Sessions
/// hergeben — nie, wer die Zeile tatsächlich schrieb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// Der letzte Claim auf die Datei trägt einen Hash, der nicht der
    /// Commit-Fassung gleicht (oder ist eine Löschung). Über die zeitliche
    /// Lage zum Commit sagt das nichts (eine Session kann mehrere Commits
    /// tragen). `later_shell`: der erste Shell-Aufruf nach diesem Claim,
    /// der die Datei erwähnt (Heuristik, kein Beleg).
    AfterAgent { later_shell: Option<LineSource> },
    /// Der letzte Claim schrieb die Datei ohne Hash (Import, redigierter
    /// Inhalt, Patch ohne Nachher-Hash) — ob der Agent genau diese Fassung
    /// schrieb, ist offen.
    Unhashed,
    /// Ein Write- oder Delete-Claim endet auf diesen Pfad, ließ sich aber
    /// diesem Checkout nicht zuordnen ([`claim_path`]) — fail-closed nicht
    /// gezählt. Warum, sagt der Reader nicht.
    Unmapped,
    /// Die letzte lesbare Witness-Beobachtung zeigt eine andere Fassung —
    /// ohne Claim, oder obwohl der letzte Claim genau diese Fassung trägt.
    WitnessOther,
    /// Kein Claim; der Witness beobachtete die Datei, aber seine letzte
    /// Beobachtung ist opak (ohne Hash) — sie bestätigt nichts.
    WitnessOpaque,
    /// Kein Claim, keine Beobachtung — ein Shell-Aufruf erwähnt die Datei
    /// (Heuristik, kein Beleg).
    Shell(LineSource),
    /// Kein Claim, keine Beobachtung, keine Erwähnung in einem Shell-Aufruf
    /// gefunden. `complete`: Die Heuristik sah alle Shell-Aufrufe (kein
    /// Budget erschöpft).
    Untouched { complete: bool },
}

/// Unerklärte Zeilen je [`Gap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GapCounts {
    /// [`Gap::AfterAgent`].
    pub after_agent: u64,
    /// [`Gap::Unhashed`].
    pub unhashed: u64,
    /// [`Gap::Unmapped`].
    pub unmapped: u64,
    /// [`Gap::WitnessOther`].
    pub witness_other: u64,
    /// [`Gap::WitnessOpaque`].
    pub witness_opaque: u64,
    /// [`Gap::Shell`].
    pub shell: u64,
    /// [`Gap::Untouched`].
    pub untouched: u64,
    /// Mindestens eine [`Gap::Untouched`]-Zählung stammt aus einer Suche,
    /// die ein Budget abbrach.
    pub untouched_incomplete: bool,
}

impl GapCounts {
    /// Die Zeilen von `gap` dazuzählen (sättigend).
    pub fn add(&mut self, gap: Gap, lines: u64) {
        let slot = match gap {
            Gap::AfterAgent { .. } => &mut self.after_agent,
            Gap::Unhashed => &mut self.unhashed,
            Gap::Unmapped => &mut self.unmapped,
            Gap::WitnessOther => &mut self.witness_other,
            Gap::WitnessOpaque => &mut self.witness_opaque,
            Gap::Shell(_) => &mut self.shell,
            Gap::Untouched { complete } => {
                self.untouched_incomplete |= !complete && lines > 0;
                &mut self.untouched
            }
        };
        *slot = slot.saturating_add(lines);
    }

    /// Alle gezählten Zeilen.
    pub fn total(&self) -> u64 {
        [
            self.after_agent,
            self.unhashed,
            self.unmapped,
            self.witness_other,
            self.witness_opaque,
            self.shell,
            self.untouched,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRecon {
    /// One-based line number in the committed file; only changed lines appear.
    pub line: u32,
    pub class: ReconClass,
    /// Der Aufruf, der diese Zeile einführte — nur auf Anfrage gerechnet
    /// ([`Claims::with_sources`]) und nur, wo ein Claim sich bis zu ihr
    /// abspielen ließ. `None` bei unerklärten Zeilen, bei reiner
    /// Dateisystem-Evidenz und über [`SOURCE_BUDGET`] hinaus.
    pub source: Option<Box<LineSource>>,
}

/// Woher eine Zeile stammt: der Schreibvorgang, der sie einführte — beim
/// Abspielen der Write-/Edit-Aufrufe von Aufruf zu Aufruf weitergetragen.
/// Eine Ableitung aus gespeicherten Sessions, kein Beweismittel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineSource {
    /// Die Session des Aufrufs.
    pub session: SessionId,
    /// Index des Turns in [`Session::turns`].
    pub turn: usize,
    /// Index des Aufrufs in [`minds_core::Turn::tool_calls`].
    pub call: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconClass {
    /// Latest witness state matches, and a tool claimed those bytes.
    Explained,
    /// Latest witness state matches, without a matching tool claim.
    ExplainedFsOnly,
    /// No witness observation; the latest tool claim matches.
    ReportedOnly,
    /// Evidence is missing or contradicts the committed state.
    Unexplained,
}

impl Reconciliation {
    /// Folds a reconciliation of further files of the same commit into this
    /// one — for callers that reconcile file by file to bound memory. Paths
    /// must be disjoint; the result stays sorted by path. Absorbing in path
    /// order costs no sort.
    pub fn absorb(&mut self, other: Reconciliation) {
        self.explained_lines += other.explained_lines;
        self.total_changed_lines += other.total_changed_lines;
        let in_order = match (self.files.last(), other.files.first()) {
            (Some(last), Some(first)) => last.path <= first.path,
            _ => true,
        };
        self.files.extend(other.files);
        if !in_order {
            self.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    /// Changed lines with some evidence: everything not `Unexplained`,
    /// including `ReportedOnly` (a tool claim, no witness).
    pub fn backed_lines(&self) -> u64 {
        self.total_changed_lines
            .saturating_sub(self.unexplained_lines())
    }

    /// Changed lines without any evidence (`Unexplained`). Where line-level
    /// attribution is unavailable, the file-level class applies to all of the
    /// file's changed lines. `ReportedOnly` lines are not counted here: they
    /// are backed by a tool claim, only not by a witness.
    pub fn unexplained_lines(&self) -> u64 {
        self.files.iter().map(FileRecon::unexplained_lines).sum()
    }

    /// Die unerklärten Zeilen, aufgeschlüsselt nach [`Gap`] — dieselbe
    /// Zählung wie [`Reconciliation::unexplained_lines`].
    pub fn unexplained_by_gap(&self) -> GapCounts {
        let mut counts = GapCounts::default();
        for file in &self.files {
            // Unerklärte Zeilen gibt es nur in unerklärten Dateien (die
            // Zeilen-Evidenz deckt eine belegte Datei ganz) — und die tragen
            // aus dem Abgleich immer einen Grund; die Tests prüfen die Summe.
            if let Some(gap) = file.gap {
                counts.add(gap, file.unexplained_lines());
            }
        }
        counts
    }
}

impl FileRecon {
    /// Changed lines of this file without any evidence; see
    /// [`Reconciliation::unexplained_lines`].
    pub fn unexplained_lines(&self) -> u64 {
        match &self.line_level {
            LineLevel::Available(lines) => lines
                .iter()
                .filter(|l| l.class == ReconClass::Unexplained)
                .count() as u64,
            LineLevel::Unavailable(_) if self.class == ReconClass::Unexplained => {
                self.changed_lines
            }
            LineLevel::Unavailable(_) => 0,
        }
    }
}

impl ReconClass {
    fn explained(self) -> bool {
        matches!(self, Self::Explained | Self::ExplainedFsOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineLevel {
    Available(Vec<LineRecon>),
    Unavailable(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Binary,
    TooLarge,
    ReconstructionMismatch,
    /// A witness reported a hash different from the commit, without bytes
    /// and without a reconstructible claim for that hash.
    MissingContent,
}

/// Caller-supplied change against the first parent. Renames must be supplied
/// as two entries: deletion of the old path and addition of the new path.
#[derive(Debug, Clone)]
pub struct ChangedFile<'a> {
    /// Unique repo-relative path within `ReconInput::changed`.
    pub path: &'a str,
    /// `None` for an addition (all files in a root commit).
    pub base: Option<&'a [u8]>,
    /// `None` for a deletion; `Some(&[])` is a present, empty file.
    pub committed: Option<&'a [u8]>,
    /// Upper bound of added/modified lines (e.g. all lines of the committed
    /// blob). Used only when a blob exceeds `LINE_LEVEL_LIMIT`, and only for
    /// an `Unexplained` file — a backed oversized file weighs one line;
    /// smaller text diffs are derived here. Deletions always count zero and
    /// binary additions/modifications one.
    pub added_lines: u64,
}

/// A witness observation. `None` means observed absence, not an unknown hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedAt {
    pub hash: Option<ContentHash>,
    pub seq: u64,
    /// RFC 3339, parsed with offset and subsecond precision for ordering.
    pub at: Option<String>,
}

/// A witness file observation (EA-08, see [`crate::observations`]). Callers
/// restrict observations to the linked sessions' window. No filesystem is
/// consulted, and `content` is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsObservation {
    pub path: String,
    pub observed: ObservedAt,
    /// Optional bytes for partial line attribution when the commit differs.
    /// Accepted only after verification against `observed.hash`.
    pub content: Option<Vec<u8>>,
    /// The file was present, but the witness deliberately recorded no hash
    /// (secret file, too large, link target outside the repository) —
    /// `observed.hash` is then `None` without meaning absence. A latest
    /// opaque observation confirms nothing and never erases an earlier
    /// readable contradiction; without one, the file is classified as if
    /// unobserved (claims only). It can therefore classify worse than claims
    /// alone — when the witness last read different bytes.
    pub opaque: bool,
}

pub struct ReconInput<'a> {
    pub commit: CommitId,
    pub base: Option<CommitId>,
    pub changed: &'a [ChangedFile<'a>],
    pub sessions: &'a [&'a Session],
    pub observations: &'a [FsObservation],
    /// Absolute repository roots of the verifying checkout. Agents record
    /// absolute effect paths (Claude Code always does); see [`claim_path`]
    /// for how a claim is mapped to repo-relative paths.
    pub roots: &'a [&'a Path],
}

/// The one repo-relative path a claimed effect path names, or `None`.
///
/// Purely lexical. A relative claim is resolved against the session's
/// recorded `cwd` when that is known and absolute, otherwise taken as
/// repo-relative. An absolute claim under one of `roots` (the verifying
/// checkout) names exactly that path.
///
/// Otherwise — another machine, worktree or symlink — the session's own
/// `cwd` and its ancestors (never `/`) are tried as the root spelling at
/// capture time, for claims below that `cwd` only. Which ancestor was the
/// repository root is not recorded, so this fallback answers only when
/// exactly **one** candidate is a path that
/// `known` confirms (callers pass "exists in the base or committed tree");
/// any ambiguity drops the claim (fail-closed). `exact_only` disables the
/// fallback — for deletions, which carry no hash that could catch a wrong
/// mapping.
pub fn claim_path(
    path: &str,
    cwd: Option<&str>,
    roots: &[&Path],
    known: &dyn Fn(&str) -> bool,
    exact_only: bool,
) -> Option<String> {
    let cwd = cwd.map(Path::new).filter(|cwd| cwd.is_absolute());
    let absolute = match cwd {
        Some(cwd) if Path::new(path).is_relative() && plain(path) => {
            cwd.join(path).to_string_lossy().into_owned()
        }
        _ => path.to_owned(),
    };
    if Path::new(&absolute).is_relative() {
        return repo_path(&absolute, &[]);
    }
    if let Some(exact) = repo_path(&absolute, roots) {
        return Some(exact);
    }
    // Only claims below cwd: anything outside it may as well lie outside the
    // repository.
    if exact_only || !cwd.is_some_and(|cwd| Path::new(&absolute).starts_with(cwd)) {
        return None;
    }
    let mut candidates: Vec<String> = cwd
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|a| a.parent().is_some())
        .filter_map(|root| repo_path(&absolute, &[root]))
        .filter(|candidate| known(candidate))
        .collect();
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn plain(path: &str) -> bool {
    !(path.starts_with('~') || path.starts_with('$') || path.contains('\\'))
}

/// The repo-relative identity of a claimed effect path, or `None` if it
/// cannot name a file of this repository. Purely lexical: no filesystem
/// access, no symlink resolution (callers pass every root spelling they
/// accept). `..`, shell expansion and backslashes never match.
pub fn repo_path(path: &str, roots: &[&Path]) -> Option<String> {
    if !plain(path) {
        return None;
    }
    let candidate = Path::new(path);
    let relative = if candidate.is_absolute() {
        roots
            .iter()
            .find_map(|root| candidate.strip_prefix(root).ok())?
    } else {
        candidate
    };
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

struct Claim<'a> {
    call: &'a ToolCall,
    effect: &'a Effect,
    source: LineSource,
    /// Position in der gemischten Turn-Reihenfolge (wie [`Mentions`]).
    order: usize,
}

/// So viele Bytes eines Shell-Aufrufs werden nach Pfaden durchsucht.
const MENTION_TEXT: usize = 64 * 1024;

/// So viele Bytes aller Shell-Aufrufe zusammen.
const MENTION_BUDGET: usize = 16 * 1024 * 1024;

/// Längere Wörter sind keine Pfade (`PATH_MAX`).
const MENTION_TOKEN: usize = 4096;

/// So viele Wörter merkt sich der Index höchstens. Ist eines der Budgets
/// erschöpft, bleibt die Heuristik für den Rest stumm — still und abhängig
/// von der Reihenfolge; dann eher [`Gap::Untouched`] als [`Gap::Shell`].
const MENTION_ENTRIES: usize = 256 * 1024;

/// So viele Wörter je Dateiname — begrenzt die Suche je unerklärter Datei
/// (viele `mod.rs`).
const MENTION_PER_NAME: usize = 4096;

/// Ein Wort eines Shell-Aufrufs, geliehen aus der gespeicherten Session.
#[derive(Clone, Copy)]
struct Mention<'a> {
    order: usize,
    source: LineSource,
    token: &'a str,
}

/// Pfadartige Wörter der Shell-Aufrufe (Exec) — für die Heuristik „ein
/// Shell-Befehl erwähnt die Datei" — und die Dateinamen von Claims, deren
/// Pfad sich nicht zuordnen ließ. Agent-neutral: reiner Text, kein Parsen
/// der Argumente. Speicher linear in der Eingabe: Jedes Wort steht einmal
/// unter seinem Dateinamen, geliehen, nie kopiert.
#[derive(Default)]
struct Mentions<'a> {
    /// Je Dateiname die Wörter, in Aufruf-Reihenfolge.
    by_name: BTreeMap<&'a str, Vec<Mention<'a>>>,
    /// Die rohen Pfade nicht zuordenbarer Claims, je Dateiname.
    unmapped: BTreeMap<&'a str, BTreeMap<&'a str, usize>>,
    spent: usize,
    entries: usize,
    /// Ein Budget griff: Die Heuristik sah nicht alles.
    truncated: bool,
}

/// Die Wörter eines Textes: getrennt an Leerraum, Anführungszeichen und
/// Shell-Zeichen. Die Argumente sind JSON: Ein `\\` mit folgendem Zeichen
/// (`\\n`, `\\"`) trennt und verschluckt das Zeichen; ein maskierter
/// Backslash (`\\\\`, ein Windows-Pfad) bleibt im Wort — solche Wörter
/// passen nie auf einen Repo-Pfad, statt als bloßer Dateiname überall.
/// `\\uXXXX` verliert nur das `u`; `\\/` (von serde_json nie erzeugt)
/// trennt. Beides ist Heuristik, nie Ausgabe.
fn words<'t>(text: &'t str, mut each: impl FnMut(&'t str)) {
    let mut start = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let split = if c == '\\' {
            if chars.peek().is_some_and(|(_, next)| *next == '\\') {
                chars.next();
                start.get_or_insert(i);
                continue;
            }
            chars.next();
            true
        } else {
            c.is_whitespace() || "'\"`;|&()<>{}[],=:".contains(c)
        };
        match (split, start) {
            (true, Some(from)) => {
                each(&text[from..i]);
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(from) = start {
        each(&text[from..]);
    }
}

/// Ob der Claim-Pfad `long` den Repo-Pfad `path` an einer Komponentengrenze
/// nennt: gleich, oder endet auf `/<path>` — `\\` zählt in `long` wie `/`
/// (ein Windows-Claim `C:\\p\\src\\a.rs` endet auf `src/a.rs`). Byteweise:
/// `/` und `\\` sind ASCII, ein Treffer liegt nie mitten in einem Zeichen.
fn names_path(long: &str, path: &str) -> bool {
    let slash = |b: u8| if b == b'\\' { b'/' } else { b };
    let (long, path) = (long.as_bytes(), path.as_bytes());
    let Some(start) = long.len().checked_sub(path.len()) else {
        return false;
    };
    long[start..].iter().zip(path).all(|(a, b)| slash(*a) == *b)
        && (start == 0 || slash(long[start - 1]) == b'/')
}

/// Der Dateiname eines Pfads (`/` oder `\\` als Trenner).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl<'a> Mentions<'a> {
    fn add(&mut self, text: &'a str, order: usize, source: LineSource) {
        if self.spent >= MENTION_BUDGET {
            self.truncated = true;
            return;
        }
        let mut end = text.len().min(MENTION_TEXT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let cut = end < text.len();
        self.truncated |= cut;
        let mut text = &text[..end];
        if cut {
            // Das letzte Wort ist womöglich angeschnitten (`a.rs.bak` →
            // `a.rs`): verwerfen.
            text = text.rfind(char::is_whitespace).map_or("", |i| &text[..i]);
        }
        self.spent = self.spent.saturating_add(text.len());
        let (by_name, entries, truncated) =
            (&mut self.by_name, &mut self.entries, &mut self.truncated);
        words(text, |token| {
            let token = token.trim_start_matches("./");
            if !(token.contains('.') || token.contains('/')) {
                return;
            }
            if token.len() > MENTION_TOKEN {
                // Ein pfadartiges Wort über `PATH_MAX`: übersprungen — die
                // Suche sah also nicht alles.
                *truncated = true;
                return;
            }
            if *entries >= MENTION_ENTRIES {
                *truncated = true;
                return;
            }
            let name = file_name(token);
            if name.is_empty() {
                return;
            }
            let seen = by_name.entry(name).or_default();
            if seen
                .last()
                .is_some_and(|m| m.order == order && m.token == token)
            {
                return;
            }
            if seen.len() >= MENTION_PER_NAME {
                *truncated = true;
                return;
            }
            seen.push(Mention {
                order,
                source,
                token,
            });
            *entries += 1;
        });
    }

    /// Die Aufrufe, die `path` erwähnen, in Aufruf-Reihenfolge: das Wort
    /// ist der Pfad, endet auf `/<path>` (ein absoluter Pfad), oder es ist
    /// der bloße Dateiname (relativ zum Arbeitsverzeichnis). `lib/mod.rs`
    /// erwähnt also nicht `src/mod.rs`.
    fn naming(&self, path: &str) -> impl Iterator<Item = &Mention<'a>> {
        let name = file_name(path);
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            // Nur `/` als Grenze: Ein Windows-Pfad (`\\`) erwähnt keinen
            // Repo-Pfad, auch keine Datei im Wurzelverzeichnis.
            .filter(move |m| {
                m.token == name
                    || m.token == path
                    || (m.token.len() > path.len()
                        && m.token.ends_with(path)
                        && m.token.as_bytes()[m.token.len() - path.len() - 1] == b'/')
            })
    }

    /// Warum `path` unerklärt ist. `state`: die Commit-Fassung (`None`:
    /// gelöscht); `contradicted`: Die letzte lesbare Beobachtung zeigt eine
    /// andere Fassung; `observed`: Es gibt überhaupt eine Beobachtung.
    fn gap(
        &self,
        path: &str,
        claims: &[Claim<'_>],
        state: Option<&ContentHash>,
        contradicted: bool,
        observed: bool,
    ) -> Gap {
        // Ein nicht zuordenbarer Claim auf diesen Pfad nach dem letzten
        // zugeordneten (oder ohne einen): Das Letzte, was eine Session mit
        // der Datei tat, ist unbekannt — nicht „danach geändert".
        let after = claims.last().map_or(0, |last| last.order);
        if self.unmapped.get(file_name(path)).is_some_and(|raws| {
            raws.iter()
                .any(|(raw, order)| *order > after && names_path(raw, path))
        }) {
            return Gap::Unmapped;
        }
        if let Some(last) = claims.last() {
            // Der Claim trägt genau diese Fassung — unerklärt nur, weil der
            // Witness zuletzt anderes sah.
            if contradicted && claims_state(std::slice::from_ref(last), state) {
                return Gap::WitnessOther;
            }
            if last.effect.kind == EffectKind::Write && last.effect.written.is_none() {
                return Gap::Unhashed;
            }
            // Je Name in Aufruf-Reihenfolge: der erste Treffer danach.
            let later_shell = self
                .naming(path)
                .find(|m| m.order > last.order)
                .map(|m| m.source);
            Gap::AfterAgent { later_shell }
        } else if contradicted {
            Gap::WitnessOther
        } else if observed {
            // Beobachtet, nicht widersprochen, trotzdem unerklärt: Die letzte
            // Beobachtung ist opak.
            Gap::WitnessOpaque
        } else if let Some(first) = self.naming(path).next() {
            Gap::Shell(first.source)
        } else {
            Gap::Untouched {
                complete: !self.truncated,
            }
        }
    }
}

/// The write and delete claims of a set of sessions, in merged turn order and
/// keyed by repo-relative path — built once per commit, then queried file by
/// file (callers that bound memory reconcile one file at a time).
#[derive(Default)]
pub struct Claims<'a> {
    by_path: BTreeMap<String, Vec<Claim<'a>>>,
    /// Die Pfade, die Shell-Aufrufe nennen (für [`Gap`]).
    mentions: Mentions<'a>,
    /// Ob je Zeile die Herkunft (`LineRecon::source`) gerechnet wird — nur
    /// auf Anfrage einer Anzeige, nie für `verify`: Es kostet einen Diff je
    /// abgespieltem Claim.
    sources: bool,
}

fn timestamp(at: Option<&str>) -> Option<jiff::Timestamp> {
    at.and_then(|at| at.parse().ok())
}

impl<'a> Claims<'a> {
    /// Collects the claims of `sessions`; `known` confirms candidate paths
    /// of the capture-time fallback (see [`claim_path`]).
    ///
    /// Turns are merged by parsed timestamp (falling back to session start),
    /// then content-addressed session identity, turn index and call index.
    /// Missing times sort first. Within a session, turn order remains
    /// authoritative even when a clock moves backwards. Ties therefore never
    /// depend on input session order.
    pub fn collect(
        sessions: &[&'a Session],
        roots: &[&Path],
        known: &dyn Fn(&str) -> bool,
    ) -> Self {
        let mut turns = Vec::new();
        for &session in sessions {
            let id = SessionId::of(session).expect("Session has a canonical representation");
            let mut time = timestamp(
                session
                    .lineage
                    .as_ref()
                    .and_then(|l| l.started_at.as_deref()),
            );
            for (turn_index, turn) in session.turns.iter().enumerate() {
                time = time.max(timestamp(turn.at.as_deref()));
                turns.push((time, id, turn_index, turn, session));
            }
        }
        turns.sort_by_key(|(time, id, index, _, _)| (*time, *id, *index));
        let mut by_path: BTreeMap<String, Vec<Claim<'a>>> = BTreeMap::new();
        let mut mentions = Mentions::default();
        let mut order = 0usize;
        for (_, id, turn_index, turn, session) in turns {
            let cwd = session.lineage.as_ref().and_then(|l| l.cwd.as_deref());
            for (call_index, call) in turn.tool_calls.iter().enumerate() {
                order += 1;
                let source = LineSource {
                    session: id,
                    turn: turn_index,
                    call: call_index,
                };
                if let Some(effect) = &call.effect
                    && effect.kind == EffectKind::Exec
                {
                    mentions.add(&call.arguments, order, source);
                }
                if let Some(effect) = &call.effect
                    && matches!(effect.kind, EffectKind::Write | EffectKind::Delete)
                    && let Some(raw) = effect.path.as_deref()
                {
                    match claim_path(raw, cwd, roots, known, effect.kind == EffectKind::Delete) {
                        Some(path) => by_path.entry(path).or_default().push(Claim {
                            call,
                            effect,
                            source,
                            order,
                        }),
                        // Nicht zuordenbar: nicht gezählt (fail-closed), aber
                        // „nichts nennt die Datei" wäre falsch.
                        None => {
                            let raws = mentions.unmapped.entry(file_name(raw)).or_default();
                            // Je Name begrenzt, gleiche Pfade nur einmal (der
                            // letzte zählt): Die Suche je Datei bleibt klein.
                            if let Some(seen) = raws.get_mut(raw) {
                                *seen = order;
                            } else if raws.len() < MENTION_PER_NAME {
                                raws.insert(raw, order);
                            } else {
                                mentions.truncated = true;
                            }
                        }
                    }
                }
            }
        }
        Self {
            by_path,
            mentions,
            sources: false,
        }
    }

    /// Rechnet beim Abgleich je Zeile die Herkunft mit (`LineRecon::source`)
    /// — für Anzeigen, die sie zeigen; `verify` braucht sie nicht.
    pub fn with_sources(mut self) -> Self {
        self.sources = true;
        self
    }

    /// The repo-relative paths with at least one write or delete claim,
    /// sorted. Claims that name no path of this repository are not here.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.by_path.keys().map(String::as_str)
    }

    /// Reconciles `changed` against these claims. No I/O, no mutation.
    pub fn reconcile(
        &self,
        commit: CommitId,
        base: Option<CommitId>,
        changed: &[ChangedFile<'_>],
        observations: &[FsObservation],
    ) -> Reconciliation {
        let mut by_path: BTreeMap<&str, Vec<&FsObservation>> = BTreeMap::new();
        for observation in observations {
            by_path
                .entry(&observation.path)
                .or_default()
                .push(observation);
        }
        for observations in by_path.values_mut() {
            observations.sort_by(|a, b| {
                let key = |o: &FsObservation| (timestamp(o.observed.at.as_deref()), o.observed.seq);
                key(a)
                    .cmp(&key(b))
                    .then_with(|| a.observed.hash.cmp(&b.observed.hash))
                    .then_with(|| a.observed.at.cmp(&b.observed.at))
                    .then_with(|| a.opaque.cmp(&b.opaque))
                    .then_with(|| a.content.cmp(&b.content))
            });
        }
        let mut result = Reconciliation {
            commit,
            base,
            files: Vec::new(),
            explained_lines: 0,
            total_changed_lines: 0,
        };
        for file in changed {
            let claims = self
                .by_path
                .get(file.path)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let observations = by_path
                .get(file.path)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (mut recon, changed, explained) =
                reconcile_file(file, claims, observations, self.sources);
            if recon.class == ReconClass::Unexplained {
                // Wie `reconcile_file`: nur die letzte lesbare Beobachtung.
                let state = (!recon.deleted).then_some(&recon.committed);
                let contradicted = observations
                    .iter()
                    .rev()
                    .find(|o| !o.opaque)
                    .is_some_and(|o| o.observed.hash.as_ref() != state);
                recon.gap = Some(self.mentions.gap(
                    file.path,
                    claims,
                    state,
                    contradicted,
                    !observations.is_empty(),
                ));
            }
            result.total_changed_lines += changed;
            result.explained_lines += explained;
            result.files.push(recon);
        }
        result.files.sort_by(|a, b| a.path.cmp(&b.path));
        result
    }
}

/// Derive reconciliation without I/O or mutation of any evidence. Without
/// access to the trees, only the changed paths confirm fallback candidates
/// (see [`claim_path`]); callers with a repository use [`Claims::collect`].
pub fn reconcile(input: &ReconInput<'_>) -> Reconciliation {
    let changed: BTreeSet<&str> = input.changed.iter().map(|f| f.path).collect();
    let known = |path: &str| changed.contains(path);
    Claims::collect(input.sessions, input.roots, &known).reconcile(
        input.commit,
        input.base,
        input.changed,
        input.observations,
    )
}

/// Wie [`reconcile`], samt Herkunft je Zeile ([`LineRecon::source`]).
pub fn reconcile_with_sources(input: &ReconInput<'_>) -> Reconciliation {
    let changed: BTreeSet<&str> = input.changed.iter().map(|f| f.path).collect();
    let known = |path: &str| changed.contains(path);
    Claims::collect(input.sessions, input.roots, &known)
        .with_sources()
        .reconcile(input.commit, input.base, input.changed, input.observations)
}

fn hash(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn claims_state(claims: &[Claim<'_>], state: Option<&ContentHash>) -> bool {
    claims.iter().any(|claim| match state {
        Some(hash) => {
            claim.effect.kind == EffectKind::Write && claim.effect.written.as_ref() == Some(hash)
        }
        None => claim.effect.kind == EffectKind::Delete,
    })
}

fn classify(
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    state: Option<&ContentHash>,
) -> ReconClass {
    if let Some(observation) = observation {
        if observation.opaque || observation.observed.hash.as_ref() != state {
            ReconClass::Unexplained
        } else if claims_state(claims, state) {
            ReconClass::Explained
        } else {
            ReconClass::ExplainedFsOnly
        }
    } else if claims
        .last()
        .is_some_and(|last| claims_state(std::slice::from_ref(last), state))
    {
        ReconClass::ReportedOnly
    } else {
        ReconClass::Unexplained
    }
}

fn reconcile_file(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observations: &[&FsObservation],
    sources: bool,
) -> (FileRecon, u64, u64) {
    let committed = hash(file.committed.unwrap_or_default());
    let state = file.committed.map(|_| &committed);
    // An opaque latest state says nothing about the bytes: fall back to the
    // claims, as without a witness — unless an earlier, readable observation
    // already contradicts the committed state. An opaque touch (a hard link,
    // a secret-looking rewrite) must not erase a contradiction.
    let observation = match observations.last().copied() {
        Some(latest) if latest.opaque => observations
            .iter()
            .rev()
            .copied()
            .find(|o| !o.opaque)
            .filter(|o| o.observed.hash.as_ref() != state),
        latest => latest,
    };
    let class = classify(claims, observation, state);
    let last_observed = observations
        .iter()
        .rev()
        .find(|o| !o.opaque && o.observed.hash.as_ref() == state)
        .map(|o| o.observed.clone());
    let (line_level, changed, explained, removes) =
        line_reconciliation(file, claims, observation, class, sources);
    (
        FileRecon {
            path: file.path.into(),
            class,
            line_level,
            committed,
            deleted: file.committed.is_none(),
            changed_lines: changed,
            removes,
            last_observed,
            gap: None,
        },
        changed,
        explained,
    )
}

fn lines_in(ranges: &[std::ops::Range<u32>]) -> u64 {
    ranges.iter().map(|r| u64::from(r.end - r.start)).sum()
}

fn line_reconciliation(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
    sources: bool,
) -> (LineLevel, u64, u64, bool) {
    let Some(committed) = file.committed else {
        return (LineLevel::Available(Vec::new()), 0, 0, file.base.is_some());
    };
    let base = file.base.unwrap_or_default();
    // Without line attribution, any change to existing bytes may remove.
    let opaque_removal = file.base.is_some_and(|b| b != committed);
    if base.contains(&0) || committed.contains(&0) {
        return (
            LineLevel::Unavailable(Reason::Binary),
            1,
            u64::from(class.explained()),
            opaque_removal,
        );
    }
    if base.len().max(committed.len()) > LINE_LEVEL_LIMIT {
        // `added_lines` is an upper bound for oversized blobs: conservative
        // only against the file. Backed files therefore weigh one line, like
        // binaries — a regenerated lockfile must not dilute unexplained
        // lines elsewhere.
        let changed = if class == ReconClass::Unexplained {
            file.added_lines
        } else {
            1
        };
        return (
            LineLevel::Unavailable(Reason::TooLarge),
            changed,
            u64::from(class.explained()),
            opaque_removal,
        );
    }
    let ranges = added_line_ranges(base, committed);
    let changed = lines_in(&ranges);
    let base_removes = removes_lines(base, committed);
    let evidence = line_evidence(file, claims, observation, class, sources);
    let Evidence {
        content,
        origins,
        class: evidence_class,
    } = match evidence {
        Ok(evidence) => evidence,
        Err(reason) => {
            return (
                LineLevel::Unavailable(reason),
                changed,
                if class.explained() { changed } else { 0 },
                base_removes,
            );
        }
    };
    // Claimed lines the commit lacks: written by the agent, then removed or
    // replaced by someone else — invisible to the added-line view below.
    let claim_removes = content
        .as_deref()
        .is_some_and(|content| removes_lines(content, committed));
    // Je Commit-Zeile die Zeile des belegten Inhalts, aus der sie unverändert
    // stammt — derselbe Algorithmus wie `added_line_ranges`: Eine Zeile passt
    // genau dann, wenn sie dort nicht als hinzugefügt gilt.
    let alignment = content
        .as_deref()
        .map(|content| minds_git::line_alignment(content, committed));
    let mut explained = 0;
    let lines: Vec<LineRecon> = ranges
        .into_iter()
        .flatten()
        .map(|line| {
            let from = alignment
                .as_ref()
                .and_then(|alignment| alignment.get(line as usize).copied().flatten());
            let class = if from.is_some() {
                evidence_class
            } else {
                ReconClass::Unexplained
            };
            explained += u64::from(class.explained());
            LineRecon {
                line: line + 1,
                class,
                source: from
                    .and_then(|from| origins.get(from as usize).copied().flatten())
                    .map(Box::new),
            }
        })
        .collect();
    // Reconstructed content already accounts for removals between base and
    // claim (the agent made them); only what vanished afterwards counts.
    let removes = match content.as_deref() {
        Some(_) => claim_removes,
        None => base_removes,
    };
    (LineLevel::Available(lines), changed, explained, removes)
}

/// Was eine Datei zeilenweise belegt: der belegte Inhalt (`None`: nichts),
/// die Herkunft je Zeile dieses Inhalts (leer, wo kein Claim abspielbar ist)
/// und die Klasse, die eine passende Zeile erhält.
struct Evidence {
    content: Option<Vec<u8>>,
    origins: Vec<Option<LineSource>>,
    class: ReconClass,
}

impl Evidence {
    fn none() -> Self {
        Self {
            content: None,
            origins: Vec::new(),
            class: ReconClass::Unexplained,
        }
    }
}

/// Die Herkunft je Zeile von `content`, wenn die Claims genau diese Fassung
/// abspielen — sonst leer. Nur für die Anzeige; die Klasse hängt nie daran.
fn origins_of(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    expected: &ContentHash,
    content: &[u8],
) -> Vec<Option<LineSource>> {
    match reconstruct(file.base, claims, expected, true) {
        Ok((bytes, origins)) if bytes == content => origins,
        _ => Vec::new(),
    }
}

fn line_evidence(
    file: &ChangedFile<'_>,
    claims: &[Claim<'_>],
    observation: Option<&FsObservation>,
    class: ReconClass,
    sources: bool,
) -> Result<Evidence, Reason> {
    if let Some(observation) = observation {
        let Some(expected) = observation
            .observed
            .hash
            .as_ref()
            .filter(|_| !observation.opaque)
        else {
            return Ok(Evidence::none());
        };
        let claimed = claims_state(claims, Some(expected));
        let evidence_class = if claimed {
            ReconClass::Explained
        } else {
            ReconClass::ExplainedFsOnly
        };
        // A matching witness hash authenticates these exact committed bytes;
        // no tool payload is needed for filesystem-only evidence.
        if class.explained() {
            let content = file.committed.map(<[u8]>::to_vec);
            let origins = match (&content, claimed) {
                (Some(content), true) if sources => origins_of(file, claims, expected, content),
                _ => Vec::new(),
            };
            return Ok(Evidence {
                content,
                origins,
                class: evidence_class,
            });
        }
        if let Some(content) = &observation.content {
            if content.len() > LINE_LEVEL_LIMIT {
                return Err(Reason::TooLarge);
            }
            if hash(content) != *expected {
                return Err(Reason::ReconstructionMismatch);
            }
            let origins = if claimed && sources {
                origins_of(file, claims, expected, content)
            } else {
                Vec::new()
            };
            return Ok(Evidence {
                content: Some(content.clone()),
                origins,
                class: evidence_class,
            });
        }
        if !claimed {
            return Err(Reason::MissingContent);
        }
        return reconstruct(file.base, claims, expected, sources).map(|(bytes, origins)| {
            Evidence {
                content: Some(bytes),
                origins,
                class: evidence_class,
            }
        });
    }
    let Some(last) = claims.last().filter(|c| c.effect.kind == EffectKind::Write) else {
        return Ok(Evidence::none());
    };
    let Some(expected) = last.effect.written.as_ref() else {
        return Ok(Evidence::none());
    };
    reconstruct(file.base, claims, expected, sources).map(|(bytes, origins)| Evidence {
        content: Some(bytes),
        origins,
        class: ReconClass::ReportedOnly,
    })
}

/// Die Herkunft je Zeile nach einem Schreibvorgang: Unverändert gebliebene
/// Zeilen behalten ihre Herkunft, neue oder ersetzte bekommen `source`.
fn carry(
    before: Option<&[u8]>,
    origins: &[Option<LineSource>],
    after: &[u8],
    source: LineSource,
) -> Vec<Option<LineSource>> {
    minds_git::line_alignment(before.unwrap_or_default(), after)
        .into_iter()
        .map(|kept| match kept {
            Some(line) => origins.get(line as usize).copied().flatten(),
            None => Some(source),
        })
        .collect()
}

/// So viele Bytes dürfen die Diffs für die Herkunft einer Datei insgesamt
/// sehen. Die Herkunft ist reine Anzeige: Darüber hinaus wird sie nicht
/// weitergetragen (dann `source: None`), die Klasse bleibt unberührt — eine
/// lange Kette großer Schreibvorgänge kann die Anzeige nicht aufhalten.
pub const SOURCE_BUDGET: usize = 16 * 1024 * 1024;

/// Zeilen eines Blobs, wie der Diff sie zählt — ohne Diff.
fn line_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
        + usize::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

/// Spielt die Claims ab und liefert die Fassung mit dem Hash `expected` —
/// mit `sources` samt der Herkunft je Zeile (Zeilen der Basis: `None`),
/// solange [`SOURCE_BUDGET`] reicht; sonst ist die Herkunft leer.
fn reconstruct(
    base: Option<&[u8]>,
    claims: &[Claim<'_>],
    expected: &ContentHash,
    sources: bool,
) -> Result<(Vec<u8>, Vec<Option<LineSource>>), Reason> {
    let mut current = base.map(<[u8]>::to_vec);
    let mut tracking = sources;
    let mut spent = 0usize;
    let mut origins: Vec<Option<LineSource>> = match base {
        Some(base) if tracking => vec![None; line_count(base)],
        _ => Vec::new(),
    };
    let mut matching = None;
    let mut failure = Reason::ReconstructionMismatch;
    for claim in claims {
        if claim.effect.kind == EffectKind::Delete {
            current = None;
            origins.clear();
            continue;
        }
        let candidate = replay(claim.call, current.as_deref());
        match candidate {
            Ok(bytes) if claim.effect.written.as_ref() == Some(&hash(&bytes)) => {
                if tracking {
                    spent = spent
                        .saturating_add(current.as_deref().map_or(0, <[u8]>::len))
                        .saturating_add(bytes.len());
                    tracking = spent <= SOURCE_BUDGET;
                }
                let next = if tracking {
                    carry(current.as_deref(), &origins, &bytes, claim.source)
                } else {
                    Vec::new()
                };
                if claim.effect.written.as_ref() == Some(expected) {
                    matching = Some((bytes.clone(), next.clone()));
                }
                current = Some(bytes);
                origins = next;
            }
            result => {
                if let Err(reason) = result {
                    failure = reason;
                }
                current = None;
                origins.clear();
            }
        }
    }
    matching.ok_or(failure)
}

/// Only reconstruct payloads with known full-write or literal-edit semantics.
/// The stored write-time hash is always the final authority, including when
/// redaction or tool-specific normalization changed the saved arguments.
fn replay(call: &ToolCall, base: Option<&[u8]>) -> Result<Vec<u8>, Reason> {
    use Reason::ReconstructionMismatch as Mismatch;
    // JSON escaping can expand each content byte up to six bytes. Bound JSON
    // parsing too, independently of the cap on the reconstructed result.
    if call.arguments.len() > LINE_LEVEL_LIMIT * 6 + 4096 {
        return Err(Reason::TooLarge);
    }
    let value: serde_json::Value = serde_json::from_str(&call.arguments).map_err(|_| Mismatch)?;
    match call.name.as_str() {
        "Write" => {
            let content = value
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or(Mismatch)?;
            if content.len() > LINE_LEVEL_LIMIT {
                return Err(Reason::TooLarge);
            }
            Ok(content.as_bytes().to_vec())
        }
        "Edit" | "MultiEdit" => {
            let mut content = std::str::from_utf8(base.ok_or(Mismatch)?)
                .map_err(|_| Mismatch)?
                .to_owned();
            if call.name == "MultiEdit" {
                let edits = value
                    .get("edits")
                    .and_then(|v| v.as_array())
                    .ok_or(Mismatch)?;
                for edit in edits {
                    content = replace(&content, edit)?;
                }
            } else {
                content = replace(&content, &value)?;
            }
            Ok(content.into_bytes())
        }
        _ => Err(Mismatch),
    }
}

fn replace(original: &str, edit: &serde_json::Value) -> Result<String, Reason> {
    use Reason::ReconstructionMismatch as Mismatch;
    let old = edit
        .get("old_string")
        .and_then(|v| v.as_str())
        .ok_or(Mismatch)?;
    let new = edit
        .get("new_string")
        .and_then(|v| v.as_str())
        .ok_or(Mismatch)?;
    let all = match edit.get("replace_all") {
        None => false,
        Some(value) => value.as_bool().ok_or(Mismatch)?,
    };
    if old.is_empty() || !original.contains(old) {
        return Err(Mismatch);
    }
    let count = if all {
        original.matches(old).count()
    } else {
        1
    };
    if original
        .len()
        .saturating_add(count.saturating_mul(new.len().saturating_sub(old.len())))
        > LINE_LEVEL_LIMIT
    {
        return Err(Reason::TooLarge);
    }
    Ok(if all {
        original.replace(old, new)
    } else {
        original.replacen(old, new, 1)
    })
}

impl crate::Index {
    /// Reconcile only the claimants of `commit` ([`crate::Index::claimants`]:
    /// trailer sessions, store-index links only without a trailer). All blobs
    /// and window-filtered witness observations are supplied by the caller.
    pub fn reconcile(
        &self,
        commit: CommitId,
        base: Option<CommitId>,
        changed: &[ChangedFile<'_>],
        observations: &[FsObservation],
        roots: &[&Path],
    ) -> Reconciliation {
        let (sessions, _) = self.claimants(commit);
        reconcile(&ReconInput {
            commit,
            base,
            changed,
            sessions: &sessions,
            observations,
            roots,
        })
    }
}
