//! Artefakt-Coverage für `minds verify` (EA-02): Wie viel des Commits ist
//! durch Evidenz erklärt — und welche Zeilen nicht?
//!
//! Die Ableitung selbst ist [`minds_reader::reconcile`] (rein, ohne I/O); hier
//! werden nur die Blobs gelesen, die verknüpften Sessions eingesammelt und das
//! Ergebnis als Text gesetzt. Nichts davon wird gespeichert (W2/W5).
//!
//! # Was „explained" in dieser Zeile heißt
//!
//! Gezählt wird alles, was **nicht** `unexplained` ist — also auch
//! `reported only` (ein Tool-Claim mit passendem Schreibzeit-Hash, aber ohne
//! Zeugen). Solange es keinen Datei-Beobachter gibt (EA-08), ist das die
//! stärkste verfügbare Aussage; die Abstufung nach Belegstärke ist Sache der
//! Assurance-Zeile (EA-12), nicht dieser Zählung.
//!
//! # Terminal-Härtung
//!
//! Pfade stammen aus dem Repository und sind fremdbestimmt — jeder Pfad läuft
//! durch [`crate::text::sanitize`], bevor er gedruckt wird. Blob-Inhalte
//! erscheinen nie in der Ausgabe.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use minds_core::Session;
use minds_git::CommitId;
use minds_reader::reconcile::{
    ChangedFile, Claims, FileRecon, LineLevel, Reason, ReconClass, Reconciliation,
};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Höchstens so viele Detailzeilen ohne `--all`.
pub(super) const DETAIL_CAP: usize = 20;

/// Die Spaltenbreite des Art-Felds einer Detailzeile (`unexplained`,
/// `file only`), wie die Achsen-Spalte des Verdikt-Blocks.
const KIND_WIDTH: usize = 15;

/// Die Reconciliation eines Commits, fertig zum Drucken.
pub(super) struct Artifact {
    recon: Reconciliation,
    /// Änderungen ohne Blob-Inhalt, die kein Claim erklären kann —
    /// Submodul-Zeiger und Modus-Wechsel: (Pfad, Grund), unentschärft.
    structural: Vec<(String, &'static str)>,
    /// `--all`: Detailzeilen nicht kappen.
    all: bool,
    /// Der Aufruf, der genau diesen Abgleich ungekappt wiederholt — Ziel
    /// und voller Commit-Hash, nie ein (womöglich mehrdeutiges) Kürzel.
    rerun: String,
}

/// Eine Detailzeile unter der Coverage-Zeile — schon entschärft.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Detail {
    kind: &'static str,
    location: String,
    reason: String,
}

/// Gleicht den Commit gegen die Claims von `sessions` ab — **nur** die
/// Sessions, deren Verdikt dieser Lauf ausspricht: Eine Session, deren
/// Integrität niemand prüft, darf keine Zeile erklären (sonst hebelte eine
/// untergeschobene Index-Kante das Gate aus). Strikt lesend.
///
/// Dateiweise: Je Datei werden beide Blobs gelesen, abgeglichen und wieder
/// freigegeben — der Speicher wächst mit der größten Datei, nicht mit dem
/// Commit. `Err(grund)`, wenn Objekte in diesem Klon fehlen — der erste
/// Elternteil (Shallow Clone) oder ein Blob (Partial Clone): Dann ist der
/// Abgleich nicht bestimmbar, und das ist ein Zustand, kein operativer
/// Fehler. Nachgeladen wird nie; `verify` bleibt offline.
pub(super) fn assess(
    ctx: &Context,
    commit: CommitId,
    sessions: &[&Session],
    all: bool,
    rerun: String,
) -> Fallible<Result<Artifact, &'static str>> {
    if let Some(parent) = ctx.repo.first_parent(commit)?
        && !ctx.repo.has_commit(parent)
    {
        return Ok(Err("first parent not in this clone (shallow)"));
    }
    // Im Partial Clone fehlen Objekte planmäßig: fehlende Bäume (Treeless
    // Clone) oder ein scheiternder Diff sind dort ein Zustand, kein Defekt.
    let partial = ctx.repo.is_partial_clone();
    const PARTIAL_TREE: &str = "tree not in this clone (partial)";
    if partial {
        let parent = ctx.repo.first_parent(commit)?;
        for c in [Some(commit), parent].into_iter().flatten() {
            if !ctx.repo.has_tree(ctx.repo.tree_of(c)?) {
                return Ok(Err(PARTIAL_TREE));
            }
        }
    }
    let changes = match ctx.repo.commit_changes(commit) {
        Ok(changes) => changes,
        Err(_) if partial => return Ok(Err(PARTIAL_TREE)),
        Err(err) => return Err(err.into()),
    };
    let present = |id: Option<minds_git::BlobId>| id.is_none_or(|id| ctx.repo.has_blob(id));
    // Nur im Partial Clone ist ein fehlender Blob ein Zustand; sonst ist er
    // ein Defekt, und das Lesen unten scheitert laut (Exit 4).
    if partial
        && !changes
            .files
            .iter()
            .all(|f| present(f.base) && present(f.committed))
    {
        return Ok(Err("blob not in this clone (partial)"));
    }
    let spellings = root_spellings(&ctx.root);
    let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
    // Einmal je Commit: der Claim-Index über alle Sessions. Kandidaten des
    // Erfassungszeit-Fallbacks bestätigt nur ein Pfad, der im Basis- oder
    // Commit-Baum steht (siehe `claim_path`).
    let changed: BTreeSet<&str> = changes.files.iter().map(|f| f.path.as_str()).collect();
    let trees = [Some(commit), changes.base]
        .into_iter()
        .flatten()
        .map(|c| ctx.repo.tree_of(c))
        .collect::<Result<Vec<_>, _>>()?;
    // Ein abgelehnter Pfad ist schlicht kein Kandidat; ein Lesefehler wird
    // gemerkt und nach dem Sammeln laut (im Partial Clone: ein Zustand).
    // Jeder Pfad wird nur einmal nachgeschlagen — lange Sessions fragen
    // dieselben Kandidaten tausendfach.
    let lookup_error = std::cell::RefCell::new(None);
    let seen = std::cell::RefCell::new(std::collections::HashMap::<String, bool>::new());
    let in_tree = |path: &str| {
        if changed.contains(path) {
            return true;
        }
        if let Some(found) = seen.borrow().get(path) {
            return *found;
        }
        let found = trees
            .iter()
            .any(|tree| match ctx.repo.path_in_tree(*tree, path) {
                Ok(found) => found,
                Err(minds_git::GitError::InvalidPath { .. }) => false,
                Err(err) => {
                    lookup_error.borrow_mut().get_or_insert(err);
                    false
                }
            });
        seen.borrow_mut().insert(path.to_owned(), found);
        found
    };
    let claims = Claims::collect(sessions, &roots, &in_tree);
    if let Some(err) = lookup_error.into_inner() {
        if partial {
            return Ok(Err(PARTIAL_TREE));
        }
        return Err(err.into());
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
        if entry.gitlink {
            structural.push((entry.path.clone(), "submodule pointer"));
        }
        if entry.mode_changed {
            structural.push((entry.path.clone(), "mode change"));
        }
        if entry.symlink {
            structural.push((entry.path.clone(), "symlink"));
        }
        // Ohne Inhaltsänderung (reiner Modus-Wechsel), ohne Blob (Gitlink)
        // oder als Symlink ist die Änderung schon strukturell benannt.
        if entry.symlink
            || entry.base == entry.committed
            || (entry.base.is_none() && entry.committed.is_none())
        {
            continue;
        }
        let base = entry.base.map(|id| ctx.repo.read_blob_id(id)).transpose()?;
        let committed = entry
            .committed
            .map(|id| ctx.repo.read_blob_id(id))
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
    Ok(Ok(Artifact {
        recon,
        structural,
        all,
        rerun,
    }))
}

/// Zeilen eines Blobs; eine letzte Zeile ohne Umbruch zählt mit.
fn line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    newlines + u64::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

/// Die Wurzel dieses Checkouts in beiden Schreibweisen — wie entdeckt und
/// kanonisch. Die Schreibweise *zur Erfassungszeit* (anderer Rechner,
/// anderer Worktree, Symlink) leitet der Reader aus dem gespeicherten `cwd`
/// jeder Session ab, rein lexikalisch ([`minds_reader::reconcile::claim_path`]).
fn root_spellings(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    out.extend(root.canonicalize().ok().filter(|c| c != root));
    out
}

impl Artifact {
    fn total(&self) -> u64 {
        self.recon.total_changed_lines
    }

    /// Geänderte Zeilen mit irgendeinem Beleg (siehe Modul-Doku).
    fn explained(&self) -> u64 {
        self.recon.backed_lines()
    }

    /// Das Segment für die Coverage-Zeile.
    pub(super) fn coverage_segment(&self) -> String {
        format!(
            "artifact {}/{} lines explained",
            self.explained(),
            self.total()
        )
    }

    /// Ganzzahliger, abgerundeter Anteil — abgerundet, damit nie „100% <
    /// required 100%" dasteht. Ein Commit ohne geänderte Zeilen ist
    /// vollständig erklärt.
    fn percent(&self) -> u64 {
        match self.total() {
            0 => 100,
            total => self.explained() * 100 / total,
        }
    }

    /// Unerklärte Änderungen, die die Prozentzahl nicht (voll) abbildet:
    /// Dateien, die Inhalt entfernen (Löschung, entfernte Zeilen), Dateien,
    /// die nur auf Datei-Ebene beurteilbar sind (binär, zu groß), unerklärte
    /// Dateien ohne unerklärte Zeile, Submodul-Zeiger und Modus-Wechsel. Eine
    /// gelöschte Prüfung wöge sonst nichts.
    fn beyond_added_lines(&self) -> usize {
        let files = self
            .recon
            .files
            .iter()
            .filter(|f| f.class == ReconClass::Unexplained)
            .filter(|f| {
                f.removes
                    || matches!(f.line_level, LineLevel::Unavailable(_))
                    || f.unexplained_lines() == 0
            })
            .count();
        files + self.structural.len()
    }

    /// Die Gate-Zeile, falls `--require-explained` verfehlt ist — sonst
    /// `None`. Verglichen wird exakt (ganzzahlig), nicht über den gerundeten
    /// Prozentwert. Jede Anforderung über 0 % verlangt außerdem, dass keine
    /// unerklärte Änderung jenseits hinzugefügter Zeilen bleibt.
    pub(super) fn gate_failure(&self, required: u8) -> Option<String> {
        let required_u64 = u64::from(required);
        if self.explained() * 100 < required_u64 * self.total() {
            return Some(format!(
                "Gate           explained {}% < required {required}%",
                self.percent()
            ));
        }
        let beyond = self.beyond_added_lines();
        (required > 0 && beyond > 0).then(|| {
            format!(
                "Gate           {beyond} unexplained change(s) beyond added lines — required {required}%"
            )
        })
    }

    /// Die Detailzeilen, gekappt auf [`DETAIL_CAP`] (außer mit `--all`).
    pub(super) fn detail_lines(&self) -> Vec<String> {
        let details = details(&self.recon.files, &self.structural);
        let shown = if self.all {
            &details[..]
        } else {
            &details[..details.len().min(DETAIL_CAP)]
        };
        let width = shown
            .iter()
            .map(|d| d.location.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<String> = shown
            .iter()
            .map(|d| {
                format!(
                    "  {:<KIND_WIDTH$}{:<width$}  {}",
                    d.kind, d.location, d.reason
                )
            })
            .collect();
        if shown.len() < details.len() {
            lines.push(format!(
                "  … {} more ({})",
                details.len() - shown.len(),
                self.rerun
            ));
        }
        lines
    }
}

/// Erst alle unerklärten Stellen (nach Pfad, dann Zeile), danach die Dateien,
/// für die nur die Datei-Ebene beurteilbar war.
fn details(files: &[FileRecon], structural: &[(String, &'static str)]) -> Vec<Detail> {
    const NOT_OBSERVED: &str = "not observed in the session";
    let mut unexplained = Vec::new();
    let mut file_only = Vec::new();
    for file in files {
        let path = shown_path(&file.path);
        let before = unexplained.len();
        if let LineLevel::Available(lines) = &file.line_level {
            let missing = lines
                .iter()
                .filter(|l| l.class == ReconClass::Unexplained)
                .map(|l| l.line);
            for (start, end) in ranges(missing) {
                let location = if start == end {
                    format!("{path}:{start}")
                } else {
                    format!("{path}:{start}-{end}")
                };
                unexplained.push(Detail {
                    kind: "unexplained",
                    location,
                    reason: NOT_OBSERVED.into(),
                });
            }
        }
        match (&file.line_level, file.class) {
            // Die ganze Datei ist ohne Beleg, Zeilen lassen sich nicht nennen.
            (LineLevel::Unavailable(reason), ReconClass::Unexplained) => {
                unexplained.push(Detail {
                    kind: "unexplained",
                    location: path,
                    reason: format!(
                        "{NOT_OBSERVED}; line level unavailable ({})",
                        reason_word(*reason)
                    ),
                });
            }
            (LineLevel::Unavailable(reason), _) => file_only.push(Detail {
                kind: "file only",
                location: path,
                reason: format!("line level unavailable ({})", reason_word(*reason)),
            }),
            // Eine Löschung, entfernte Zeilen (auch neben unerklärten
            // Zeilen — sonst zeigte die Liste nur den Kommentar, nicht die
            // entfernte Prüfung) oder unerklärt ohne unerklärte Zeile.
            (LineLevel::Available(_), ReconClass::Unexplained)
                if file.deleted || file.removes || unexplained.len() == before =>
            {
                unexplained.push(Detail {
                    kind: "unexplained",
                    location: path,
                    reason: if file.deleted {
                        "deletion not observed in the session".into()
                    } else if file.removes {
                        "removed lines not observed in the session".into()
                    } else {
                        NOT_OBSERVED.into()
                    },
                });
            }
            (LineLevel::Available(_), _) => {}
        }
    }
    for (path, what) in structural {
        unexplained.push(Detail {
            kind: "unexplained",
            location: shown_path(path),
            reason: format!("{what} not observed in the session"),
        });
    }
    unexplained.extend(file_only);
    unexplained
}

/// Ein Pfad zur Anzeige: entschärft und auf [`PATH_CAP`] Zeichen gekürzt —
/// Git erlaubt beliebig lange Namen, und die Spaltenbreite folgt dem
/// längsten.
fn shown_path(path: &str) -> String {
    let path = crate::text::sanitize(path);
    if path.chars().count() <= PATH_CAP {
        return path;
    }
    let tail: String = path.chars().rev().take(PATH_CAP - 1).collect();
    format!("…{}", tail.chars().rev().collect::<String>())
}

/// Höchstens so viele Zeichen eines Pfads in einer Detailzeile.
const PATH_CAP: usize = 256;

/// Fasst aufsteigende Zeilennummern zu geschlossenen Bereichen zusammen.
fn ranges(lines: impl Iterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for line in lines {
        match out.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(line) => *end = line,
            _ => out.push((line, line)),
        }
    }
    out
}

fn reason_word(reason: Reason) -> &'static str {
    match reason {
        Reason::Binary => "binary",
        Reason::TooLarge => "too large",
        Reason::ReconstructionMismatch => "reconstruction mismatch",
        Reason::MissingContent => "content missing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::ContentHash;
    use minds_reader::reconcile::LineRecon;

    fn file(path: &str, class: ReconClass, line_level: LineLevel) -> FileRecon {
        FileRecon {
            path: path.into(),
            class,
            line_level,
            committed: ContentHash::from_bytes([0; 32]),
            deleted: false,
            changed_lines: 0,
            removes: false,
            last_observed: None,
        }
    }

    fn lines(spec: &[(u32, ReconClass)]) -> LineLevel {
        LineLevel::Available(
            spec.iter()
                .map(|&(line, class)| LineRecon { line, class })
                .collect(),
        )
    }

    fn artifact(files: Vec<FileRecon>, total: u64, all: bool) -> Artifact {
        Artifact {
            recon: Reconciliation {
                commit: "1234567890123456789012345678901234567890".parse().unwrap(),
                base: None,
                files,
                explained_lines: 0,
                total_changed_lines: total,
            },
            structural: Vec::new(),
            all,
            rerun: "minds verify 1234567890123456789012345678901234567890 --all".into(),
        }
    }

    /// Die Golden-Form aus der Spec: Bereiche komprimiert, Spalten
    /// ausgerichtet, unerklärte Stellen vor den Nur-Datei-Einträgen.
    #[test]
    fn detail_lines_match_the_golden_layout() {
        use ReconClass::*;
        let mut merge = file(
            "src/sort/merge.rs",
            Unexplained,
            lines(&[
                (87, ReportedOnly),
                (88, Unexplained),
                (89, ReportedOnly),
                (91, Unexplained),
                (92, Unexplained),
            ]),
        );
        merge.changed_lines = 5;
        let lock = file(
            "Cargo.lock",
            ReportedOnly,
            LineLevel::Unavailable(Reason::ReconstructionMismatch),
        );
        let a = artifact(vec![lock, merge], 150, false);
        assert_eq!(
            a.detail_lines().join("\n"),
            "  unexplained    src/sort/merge.rs:88     not observed in the session\n  \
             unexplained    src/sort/merge.rs:91-92  not observed in the session\n  \
             file only      Cargo.lock               line level unavailable (reconstruction mismatch)"
        );
        assert_eq!(a.coverage_segment(), "artifact 147/150 lines explained");
    }

    #[test]
    fn whole_file_gaps_and_deletions_are_named() {
        use ReconClass::*;
        let mut deleted = file("gone.rs", Unexplained, lines(&[]));
        deleted.deleted = true;
        let mut binary = file(
            "logo.png",
            Unexplained,
            LineLevel::Unavailable(Reason::Binary),
        );
        binary.changed_lines = 1;
        let removal = file("trim.rs", Unexplained, lines(&[]));
        let a = artifact(vec![deleted, binary, removal], 1, true);
        assert_eq!(
            a.detail_lines(),
            [
                "  unexplained    gone.rs   deletion not observed in the session",
                "  unexplained    logo.png  not observed in the session; line level unavailable (binary)",
                "  unexplained    trim.rs   not observed in the session",
            ]
        );
        assert_eq!(a.coverage_segment(), "artifact 0/1 lines explained");
    }

    #[test]
    fn hostile_paths_are_sanitized() {
        let f = file(
            "evil\u{1b}[31m\u{202e}.rs",
            ReconClass::Unexplained,
            lines(&[(1, ReconClass::Unexplained)]),
        );
        let out = artifact(vec![f], 1, false).detail_lines().join("\n");
        assert!(!out.contains(['\u{1b}', '\u{202e}']), "{out}");
    }

    #[test]
    fn gate_compares_exactly_and_rounds_down() {
        use ReconClass::*;
        let mut f = file("a", Unexplained, lines(&[(1, Unexplained)]));
        f.changed_lines = 150;
        let a = artifact(vec![f], 150, false);
        assert_eq!(
            a.gate_failure(100).as_deref(),
            Some("Gate           explained 99% < required 100%")
        );
        assert_eq!(a.gate_failure(99), None);
        assert_eq!(a.gate_failure(0), None);
        let empty = artifact(vec![], 0, false);
        assert_eq!(empty.gate_failure(100), None);
        assert_eq!(empty.coverage_segment(), "artifact 0/0 lines explained");
    }

    /// Löschungen, reine Entfernungen und Datei-Ebene-Funde wiegen in der
    /// Prozentzahl nichts oder fast nichts — sie lassen jedes Gate über 0 %
    /// trotzdem scheitern.
    #[test]
    fn weightless_unexplained_files_fail_any_positive_gate() {
        use ReconClass::*;
        let mut deleted = file("auth.rs", Unexplained, lines(&[]));
        deleted.deleted = true;
        let mut explained = file("a.rs", ReportedOnly, lines(&[(1, ReportedOnly)]));
        explained.changed_lines = 1;
        let a = artifact(vec![deleted, explained], 1, false);
        assert_eq!(a.coverage_segment(), "artifact 1/1 lines explained");
        assert_eq!(
            a.gate_failure(1).as_deref(),
            Some("Gate           1 unexplained change(s) beyond added lines — required 1%")
        );
        assert_eq!(a.gate_failure(0), None);

        let mut binary = file("blob", Unexplained, LineLevel::Unavailable(Reason::Binary));
        binary.changed_lines = 1;
        let mut text = file("a.rs", ReportedOnly, lines(&[(1, ReportedOnly)]));
        text.changed_lines = 500;
        let a = artifact(vec![binary, text], 501, false);
        assert!(a.gate_failure(99).is_some());

        // Ein unerklärter Bereich mit Zeilengewicht zählt nur prozentual —
        // entfernt dieselbe Datei aber auch Zeilen, fällt jedes Gate.
        let mut partial = file(
            "b.rs",
            Unexplained,
            lines(&[(1, ReportedOnly), (2, Unexplained)]),
        );
        partial.changed_lines = 2;
        assert_eq!(
            artifact(vec![partial.clone()], 2, false).gate_failure(50),
            None
        );
        partial.removes = true;
        let a = artifact(vec![partial], 2, false);
        assert!(a.gate_failure(50).is_some());
        assert_eq!(
            a.detail_lines(),
            [
                "  unexplained    b.rs:2  not observed in the session",
                "  unexplained    b.rs    removed lines not observed in the session",
            ]
        );

        // Submodul-Zeiger und Modus-Wechsel haben keinen Inhalt, aber Gewicht.
        let mut a = artifact(vec![], 0, false);
        a.structural
            .push(("vendor/crypto".into(), "submodule pointer"));
        assert!(a.gate_failure(1).is_some());
        assert_eq!(
            a.detail_lines(),
            ["  unexplained    vendor/crypto  submodule pointer not observed in the session"]
        );
    }

    #[test]
    fn line_count_counts_a_final_partial_line() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(line_count(b"a\nb\n"), 2);
    }

    #[test]
    fn overlong_paths_are_cut_from_the_front() {
        let long = format!("{}/x.rs", "d".repeat(1000));
        let shown = shown_path(&long);
        assert_eq!(shown.chars().count(), PATH_CAP);
        assert!(shown.starts_with('…') && shown.ends_with("/x.rs"));
        assert_eq!(shown_path("a.rs"), "a.rs");
    }

    #[test]
    fn ranges_compress_runs() {
        assert_eq!(
            ranges([1, 2, 3, 5, 7, 8].into_iter()),
            [(1, 3), (5, 5), (7, 8)]
        );
        assert_eq!(ranges(std::iter::empty()), []);
    }
}
