//! Die Artefakt-Reconciliation eines Commits — die I/O-Seite zu
//! [`crate::reconcile`], geteilt von `minds verify`, der TUI und
//! `minds render`.
//!
//! [`crate::reconcile`] ist rein: Blobs rein, Klassen raus. Hier werden die
//! Blobs gelesen — dateiweise, damit der Speicher mit der größten Datei
//! wächst, nicht mit dem Commit — und die Zustände benannt, in denen ein
//! Abgleich nicht bestimmbar ist (Shallow und Partial Clone). Strikt lesend:
//! keine Ref-Bewegung, kein Store-Zugriff, nichts wird gespeichert (W2/W5).
//! Nachgeladen wird nie.
//!
//! Dazu das gemeinsame Vokabular der Oberflächen ([`summary`],
//! [`NOT_OBSERVED`], [`ReconClass::word`], [`Reason::word`]), damit CLI, TUI
//! und HTML dieselben Worte sprechen. Pfade im Ergebnis sind Identitäten,
//! kein Anzeigetext — jede Oberfläche entschärft sie an ihrer Senke.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use minds_core::Session;
use minds_git::{BlobId, CommitId, GitError, Repo};

use crate::reconcile::{
    ChangedFile, Claims, FileRecon, LineLevel, Reason, ReconClass, Reconciliation,
};

/// Der Legendentext einer unerklärten Stelle — nie ein Fehler, nur: In der
/// Session nicht beobachtet.
pub const NOT_OBSERVED: &str = "not observed in the session";

/// Eine Änderung ohne Blob-Inhalt, die kein Claim erklären kann.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Structural {
    /// Repo-relativer Pfad, unentschärft.
    pub path: String,
    /// Was sich geändert hat: `submodule pointer`, `mode change`, `symlink`.
    pub what: &'static str,
}

/// Ein abgeglichener Commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessed {
    /// Die Klassen je Datei und Zeile.
    pub recon: Reconciliation,
    /// Submodul-Zeiger, Modus-Wechsel und Symlinks — in Pfad-Reihenfolge
    /// des Diffs.
    pub structural: Vec<Structural>,
}

/// Gleicht `commit` gegen die Claims von `sessions` ab.
///
/// `roots` sind die Schreibweisen der Wurzel des prüfenden Checkouts (siehe
/// [`roots_of`]). Der Aufrufer wählt die Sessions: Nur wessen Claims hier
/// stehen, kann eine Zeile erklären.
///
/// `Ok(Err(grund))`, wenn Objekte in diesem Klon fehlen — der erste
/// Elternteil (Shallow Clone) oder ein Baum bzw. Blob (Partial Clone): Das
/// ist ein Zustand, kein Defekt. Jeder andere Lesefehler ist `Err`.
pub fn assess(
    repo: &Repo,
    roots: &[&Path],
    commit: CommitId,
    sessions: &[&Session],
) -> minds_git::Result<Result<Assessed, &'static str>> {
    if let Some(parent) = repo.first_parent(commit)?
        && !repo.has_commit(parent)
    {
        return Ok(Err("first parent not in this clone (shallow)"));
    }
    // Im Partial Clone fehlen Objekte planmäßig: fehlende Bäume (Treeless
    // Clone) oder ein scheiternder Diff sind dort ein Zustand, kein Defekt.
    let partial = repo.is_partial_clone();
    const PARTIAL_TREE: &str = "tree not in this clone (partial)";
    if partial {
        let parent = repo.first_parent(commit)?;
        for c in [Some(commit), parent].into_iter().flatten() {
            if !repo.has_tree(repo.tree_of(c)?) {
                return Ok(Err(PARTIAL_TREE));
            }
        }
    }
    let changes = match repo.commit_changes(commit) {
        Ok(changes) => changes,
        Err(_) if partial => return Ok(Err(PARTIAL_TREE)),
        Err(err) => return Err(err),
    };
    let present = |id: Option<BlobId>| id.is_none_or(|id| repo.has_blob(id));
    // Nur im Partial Clone ist ein fehlender Blob ein Zustand; sonst ist er
    // ein Defekt, und das Lesen unten scheitert laut.
    if partial
        && !changes
            .files
            .iter()
            .all(|f| present(f.base) && present(f.committed))
    {
        return Ok(Err("blob not in this clone (partial)"));
    }
    // Einmal je Commit: der Claim-Index über alle Sessions. Kandidaten des
    // Erfassungszeit-Fallbacks bestätigt nur ein Pfad, der im Basis- oder
    // Commit-Baum steht (siehe `claim_path`).
    let changed: BTreeSet<&str> = changes.files.iter().map(|f| f.path.as_str()).collect();
    let trees = [Some(commit), changes.base]
        .into_iter()
        .flatten()
        .map(|c| repo.tree_of(c))
        .collect::<Result<Vec<_>, _>>()?;
    // Ein abgelehnter Pfad ist schlicht kein Kandidat; ein Lesefehler wird
    // gemerkt und nach dem Sammeln laut (im Partial Clone: ein Zustand).
    // Jeder Pfad wird nur einmal nachgeschlagen — lange Sessions fragen
    // dieselben Kandidaten tausendfach.
    let lookup_error = RefCell::new(None);
    let seen = RefCell::new(HashMap::<String, bool>::new());
    let in_tree = |path: &str| {
        if changed.contains(path) {
            return true;
        }
        if let Some(found) = seen.borrow().get(path) {
            return *found;
        }
        let found = trees
            .iter()
            .any(|tree| match repo.path_in_tree(*tree, path) {
                Ok(found) => found,
                Err(GitError::InvalidPath { .. }) => false,
                Err(err) => {
                    lookup_error.borrow_mut().get_or_insert(err);
                    false
                }
            });
        seen.borrow_mut().insert(path.to_owned(), found);
        found
    };
    let claims = Claims::collect(sessions, roots, &in_tree);
    if let Some(err) = lookup_error.into_inner() {
        if partial {
            return Ok(Err(PARTIAL_TREE));
        }
        return Err(err);
    }
    let no_claims = Claims::default();
    let mut recon = Reconciliation {
        commit,
        base: changes.base,
        files: Vec::new(),
        explained_lines: 0,
        total_changed_lines: 0,
    };
    let mut structural = Vec::new();
    for entry in &changes.files {
        for (flag, what) in [
            (entry.gitlink, "submodule pointer"),
            (entry.mode_changed, "mode change"),
            (entry.symlink, "symlink"),
        ] {
            if flag {
                structural.push(Structural {
                    path: entry.path.clone(),
                    what,
                });
            }
        }
        // Ohne Inhaltsänderung (reiner Modus-Wechsel), ohne Blob (Gitlink)
        // oder als Symlink ist die Änderung schon strukturell benannt.
        if entry.symlink
            || entry.base == entry.committed
            || (entry.base.is_none() && entry.committed.is_none())
        {
            continue;
        }
        let base = entry.base.map(|id| repo.read_blob_id(id)).transpose()?;
        let committed = entry
            .committed
            .map(|id| repo.read_blob_id(id))
            .transpose()?;
        let file = ChangedFile {
            path: &entry.path,
            base: base.as_deref(),
            committed: committed.as_deref(),
            // Nur für Dateien über der Zeilen-Grenze maßgeblich: alle Zeilen
            // des Commit-Stands, eine obere Schranke — nie Gits `--numstat`,
            // das `-diff`-Attribute zu „0 Zeilen" machen.
            added_lines: committed.as_deref().map_or(0, line_count),
        };
        // Ein nicht als UTF-8 lesbarer Pfad ist nur Anzeigeform und kann mit
        // anderen kollidieren: Kein Claim darf ihn erklären.
        let claims = if entry.path_is_utf8 {
            &claims
        } else {
            &no_claims
        };
        // Ohne Datei-Beobachter (EA-08) gibt es keine Beobachtungen.
        recon.absorb(claims.reconcile(commit, changes.base, std::slice::from_ref(&file), &[]));
    }
    Ok(Ok(Assessed { recon, structural }))
}

/// Höchstens so viele Commits einer Session gleicht der Evidence-Mode ab —
/// jeder Abgleich liest Blobs, synchron beim Öffnen. Der Rest wird als
/// „nicht abgeglichen" benannt, nie verschwiegen.
pub const MAX_COMMITS: usize = 20;

/// Der Abgleich eines Commits, wie eine Oberfläche ihn zeigt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitArtifact {
    /// Der Commit.
    pub commit: CommitId,
    /// Sein Betreff, entschärft — sofern bekannt.
    pub subject: Option<String>,
    /// Die Claims stammen aus Store-Index-Kanten, nicht aus einem Trailer —
    /// vermutete Verknüpfungen; die Oberfläche sagt das dazu.
    pub inferred: bool,
    /// Wie viele Sessions Claims beitragen. Der Abgleich gilt dem **Commit**:
    /// Bei mehr als einer erklären auch die Claims der anderen Sessions.
    pub claimants: usize,
    /// Das Ergebnis.
    pub state: ArtifactState,
}

/// Was der Abgleich eines Commits ergab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactState {
    /// Abgeglichen.
    Assessed(Assessed),
    /// Nicht bestimmbar — ein Zustand des Klons (Shallow, Partial).
    Unavailable(&'static str),
    /// Ein Lesefehler, schon entschärft.
    Failed(String),
}

/// Die Text-Kennzeichnung eines Abgleichs ohne lesbare Claimant-Session.
/// Ein Schutz für direkte Aufrufer von [`crate::Index::artifact`]: Über
/// [`crate::Index::claimed_commits`] ist die Session selbst immer Claimant.
pub const NO_CLAIMANT_NOTE: &str = "no readable claimant session";

/// Die Text-Kennzeichnung vermuteter Claimants.
pub const INFERRED_NOTE: &str = "claims from inferred links (no trailer)";

impl crate::Index {
    /// Die Sessions, deren Claims `commit` erklären dürfen, und ob sie nur
    /// vermutet verknüpft sind — dieselbe Wahl wie `minds verify <rev>`:
    /// Nennt der **Trailer** Sessions, zählen genau diese (fehlt eine im
    /// Index, trägt sie nichts bei — es gibt keinen Rückfall). Nur ohne
    /// Trailer zählen die Kanten des Store-Index. Ob eine Store-Kante sich
    /// selbst `Observed` nennt, spielt keine Rolle: Sie liegt auf einem
    /// geteilten Ref, der Trailer im Commit.
    ///
    /// Eine Abweichung bleibt: Sessions ohne erfasste Absicht hält der Index
    /// gar nicht (siehe [`crate::Index::build`]); `verify` liest sie direkt
    /// aus dem Store.
    ///
    /// Ein Commit außerhalb von HEAD (Seitenzweig, geholter PR-Ref) zählt mit
    /// seinem Trailer als nicht vermutet — der Trailer ist Commit-Inhalt,
    /// aber dieser Stand ist nicht gemergte Historie ([`crate::Index::position`]
    /// ist dann `None`).
    ///
    /// Auch Commits außerhalb von HEAD tragen ihren gelesenen Trailer (siehe
    /// [`crate::Index::build`]); ist ihre Message unlesbar, gibt es keine
    /// Claimants — unbekannt heißt nicht „ohne Trailer".
    pub fn claimants(&self, commit: CommitId) -> (Vec<&Session>, bool) {
        if self.trailer_unknown(commit) {
            return (Vec::new(), false);
        }
        let trailer = self.trailer_ids(commit);
        let (ids, inferred) = if trailer.is_empty() {
            (self.sessions_of(commit), true)
        } else {
            (trailer, false)
        };
        let sessions: Vec<&Session> = ids.iter().filter_map(|id| self.session(*id)).collect();
        let inferred = inferred && !sessions.is_empty();
        (sessions, inferred)
    }

    /// Gleicht `commit` gegen die Claims seiner [`Index::claimants`] ab.
    /// Fail-soft: Ein Lesefehler wird zu [`ArtifactState::Failed`].
    pub fn artifact(&self, repo: &Repo, roots: &[&Path], commit: CommitId) -> CommitArtifact {
        let (claimants, inferred) = self.claimants(commit);
        let state = match assess(repo, roots, commit, &claimants) {
            Ok(Ok(assessed)) => ArtifactState::Assessed(assessed),
            Ok(Err(why)) => ArtifactState::Unavailable(why),
            Err(err) => ArtifactState::Failed(crate::sanitize(&err.to_string())),
        };
        CommitArtifact {
            commit,
            subject: self.subject_of(commit).map(str::to_owned),
            inferred,
            claimants: claimants.len(),
            state,
        }
    }
}

impl crate::Index {
    /// Die Commits, an deren Abgleich `id` als Claimant teilnimmt: die, deren
    /// Trailer sie nennt, und — vermutet — die per Store-Index verknüpften
    /// **ohne** Trailer. Einen Commit mit fremdem Trailer gleicht sie nicht
    /// ab: Dort zählen ihre Claims nicht ([`Index::claimants`]).
    ///
    /// In Revwalk-Reihenfolge ab HEAD; Commits außerhalb von HEAD danach,
    /// nach Hash. Commits mit unbekanntem Trailer (nicht im Klon, Message
    /// unlesbar) fehlen — ihr Abgleich wäre ohne Claimants nicht bestimmbar.
    pub fn claimed_commits(&self, id: minds_core::SessionId) -> Vec<CommitId> {
        let mut commits: Vec<CommitId> = self
            .commits_of(id)
            .into_iter()
            .filter(|commit| !self.trailer_unknown(*commit))
            .filter(|commit| {
                let trailer = self.trailer_ids(*commit);
                trailer.is_empty() || trailer.contains(&id)
            })
            .collect();
        commits.sort_by_key(|commit| (self.position(*commit).unwrap_or(usize::MAX), *commit));
        commits
    }
}

impl crate::Inspection {
    /// Die Abgleiche der Commits, an denen `id` als Claimant teilnimmt
    /// ([`crate::Index::claimed_commits`]) — für den Evidence-Mode. Leer,
    /// wenn es keinen gibt. Synchron; über [`MAX_COMMITS`] hinaus wird
    /// nichts gelesen, diese Commits stehen als nicht abgeglichen da.
    pub fn artifacts(&self, repo: &Repo, id: minds_core::SessionId) -> Vec<CommitArtifact> {
        let spellings = roots_of(repo);
        let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
        let index = self.index();
        index
            .claimed_commits(id)
            .into_iter()
            .enumerate()
            .map(|(i, commit)| {
                if i < MAX_COMMITS {
                    index.artifact(repo, &roots, commit)
                } else {
                    // Ohne I/O: Wer beitrüge, ist trotzdem bekannt.
                    let (claimants, inferred) = index.claimants(commit);
                    CommitArtifact {
                        commit,
                        subject: index.subject_of(commit).map(str::to_owned),
                        inferred,
                        claimants: claimants.len(),
                        state: ArtifactState::Unavailable("skipped: too many linked commits"),
                    }
                }
            })
            .collect()
    }
}

/// Der Kopf-Zusatz eines Abgleichs: vermutete Claimants und mehrere
/// beitragende Sessions — leer, wenn es nichts zu sagen gibt.
pub fn provenance_note(artifact: &CommitArtifact) -> String {
    let mut parts = Vec::new();
    // Der Trailer nennt Sessions, aber keine ist lesbar (ohne Absicht,
    // vergessen, beschädigt): Dann erklärt niemand — und das wird gesagt.
    if artifact.claimants == 0 && matches!(artifact.state, ArtifactState::Assessed(_)) {
        parts.push(NO_CLAIMANT_NOTE.to_owned());
    }
    if artifact.claimants > 1 {
        parts.push(format!("claims of {} sessions", artifact.claimants));
    }
    if artifact.inferred {
        parts.push(INFERRED_NOTE.to_owned());
    }
    parts.join(" · ")
}

/// Zeilen eines Blobs; eine letzte Zeile ohne Umbruch zählt mit.
pub fn line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    newlines + u64::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

/// Die Wurzel eines Checkouts in beiden Schreibweisen — wie entdeckt und
/// kanonisch. Die Schreibweise *zur Erfassungszeit* (anderer Rechner,
/// anderer Worktree, Symlink) leitet der Reader aus dem gespeicherten `cwd`
/// jeder Session ab, rein lexikalisch ([`crate::reconcile::claim_path`]).
pub fn root_spellings(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    out.extend(root.canonicalize().ok().filter(|c| c != root));
    out
}

/// Die Arbeitsbaum-Wurzel eines Repositorys in beiden Schreibweisen — das
/// Verzeichnis über dem Git-Verzeichnis (siehe [`root_spellings`]).
pub fn roots_of(repo: &Repo) -> Vec<PathBuf> {
    root_spellings(repo.git_dir().parent().unwrap_or_else(|| repo.git_dir()))
}

/// Die Zusammenfassung: `artifact X/Y lines explained`. „Explained" zählt
/// alles nicht Unerklärte, auch `reported only` — ohne Datei-Beobachter
/// (EA-08) die stärkste verfügbare Aussage; die Abstufung nach Belegstärke
/// ist Sache der Assurance (EA-12).
pub fn summary(recon: &Reconciliation) -> String {
    format!(
        "artifact {}/{} lines explained",
        recon.backed_lines(),
        recon.total_changed_lines
    )
}

/// Fasst aufsteigende Zeilennummern zu geschlossenen Bereichen zusammen.
pub fn ranges(lines: impl Iterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for line in lines {
        match out.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(line) => *end = line,
            _ => out.push((line, line)),
        }
    }
    out
}

/// Ein Bereich als Text: `7` oder `7-9`.
pub fn range_text((start, end): (u32, u32)) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

impl FileRecon {
    /// Die unerklärten Zeilen (1-basiert, im Commit-Stand) als Bereiche —
    /// leer, wo die Zeilen-Ebene nicht verfügbar ist.
    pub fn unexplained_ranges(&self) -> Vec<(u32, u32)> {
        match &self.line_level {
            LineLevel::Available(lines) => ranges(
                lines
                    .iter()
                    .filter(|l| l.class == ReconClass::Unexplained)
                    .map(|l| l.line),
            ),
            LineLevel::Unavailable(_) => Vec::new(),
        }
    }

    /// Die unerklärten Zeilennummern im Commit-Stand.
    pub fn unexplained_set(&self) -> BTreeSet<u32> {
        match &self.line_level {
            LineLevel::Available(lines) => lines
                .iter()
                .filter(|l| l.class == ReconClass::Unexplained)
                .map(|l| l.line)
                .collect(),
            LineLevel::Unavailable(_) => BTreeSet::new(),
        }
    }

    /// Ein Satz zur Datei neben ihrer Klasse: was unerklärt ist oder warum
    /// die Zeilen-Ebene fehlt. Leer, wenn es nichts zu sagen gibt.
    pub fn note(&self) -> String {
        if self.deleted {
            return if self.class == ReconClass::Unexplained {
                format!("deletion {NOT_OBSERVED}")
            } else {
                "deleted".into()
            };
        }
        if let LineLevel::Unavailable(reason) = &self.line_level {
            return if self.class == ReconClass::Unexplained {
                format!("{NOT_OBSERVED}; line level unavailable ({})", reason.word())
            } else {
                format!("line level unavailable ({})", reason.word())
            };
        }
        let mut parts = Vec::new();
        let ranges = self.unexplained_ranges();
        if !ranges.is_empty() {
            let word = if ranges.len() == 1 && ranges[0].0 == ranges[0].1 {
                "line"
            } else {
                "lines"
            };
            let list: Vec<String> = ranges.into_iter().map(range_text).collect();
            parts.push(format!("{word} {} {NOT_OBSERVED}", list.join(", ")));
        }
        if self.class == ReconClass::Unexplained {
            // Entfernte Zeilen auch neben unerklärten Zeilen — sonst zeigte
            // die Liste nur den Kommentar, nicht die entfernte Prüfung.
            if self.removes {
                parts.push(format!("removed lines {NOT_OBSERVED}"));
            } else if parts.is_empty() {
                parts.push(NOT_OBSERVED.into());
            }
        }
        parts.join("; ")
    }
}

impl ReconClass {
    /// Das Wort aus dem gemeinsamen Vokabular (`00-conventions`).
    pub fn word(self) -> &'static str {
        match self {
            Self::Explained => "explained",
            Self::ExplainedFsOnly => "explained (fs only)",
            Self::ReportedOnly => "reported only",
            Self::Unexplained => "unexplained",
        }
    }
}

impl Reason {
    /// Warum die Zeilen-Ebene fehlt, als Wort.
    pub fn word(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::TooLarge => "too large",
            Self::ReconstructionMismatch => "reconstruction mismatch",
            Self::MissingContent => "content missing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_compress_runs() {
        assert_eq!(
            ranges([1, 2, 3, 5, 7, 8].into_iter()),
            [(1, 3), (5, 5), (7, 8)]
        );
        assert_eq!(ranges(std::iter::empty()), []);
        assert_eq!(
            ranges([u32::MAX - 1, u32::MAX].into_iter()),
            [(u32::MAX - 1, u32::MAX)]
        );
    }

    #[test]
    fn line_count_counts_a_final_partial_line() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(line_count(b"a\nb\n"), 2);
    }

    fn file(class: ReconClass, line_level: LineLevel) -> FileRecon {
        FileRecon {
            path: "a".into(),
            class,
            line_level,
            committed: minds_core::ContentHash::from_bytes([0; 32]),
            deleted: false,
            changed_lines: 1,
            removes: false,
            last_observed: None,
        }
    }

    /// Dieselben Worte wie die Detailzeilen von `minds verify`.
    #[test]
    fn notes_speak_the_verify_vocabulary() {
        use crate::reconcile::LineRecon;
        use ReconClass::*;
        let binary = file(Unexplained, LineLevel::Unavailable(Reason::Binary));
        assert_eq!(
            binary.note(),
            "not observed in the session; line level unavailable (binary)"
        );
        let lock = file(ReportedOnly, LineLevel::Unavailable(Reason::TooLarge));
        assert_eq!(lock.note(), "line level unavailable (too large)");
        let lines = LineLevel::Available(vec![
            LineRecon {
                line: 2,
                class: Unexplained,
            },
            LineRecon {
                line: 3,
                class: Unexplained,
            },
            LineRecon {
                line: 5,
                class: Unexplained,
            },
        ]);
        let mut both = file(Unexplained, lines);
        assert_eq!(both.note(), "lines 2-3, 5 not observed in the session");
        both.removes = true;
        assert_eq!(
            both.note(),
            "lines 2-3, 5 not observed in the session; removed lines not observed in the session"
        );
        let mut gone = file(Unexplained, LineLevel::Available(Vec::new()));
        gone.deleted = true;
        assert_eq!(gone.note(), "deletion not observed in the session");
        assert_eq!(file(Explained, LineLevel::Available(Vec::new())).note(), "");
    }
}
