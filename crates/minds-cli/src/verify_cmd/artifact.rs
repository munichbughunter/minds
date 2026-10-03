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

use std::path::{Path, PathBuf};

use minds_core::Session;
use minds_git::CommitId;
use minds_reader::artifact::{NOT_OBSERVED, Structural, range_text, ranges, root_spellings};
use minds_reader::reconcile::{FileRecon, LineLevel, ReconClass, Reconciliation};

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
    /// Submodul-Zeiger und Modus-Wechsel, unentschärft.
    structural: Vec<Structural>,
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
/// Das Lesen selbst ist [`minds_reader::artifact::assess`] (dateiweise,
/// offline). `Err(grund)`, wenn Objekte in diesem Klon fehlen — dann ist der
/// Abgleich nicht bestimmbar, und das ist ein Zustand, kein operativer
/// Fehler.
pub(super) fn assess(
    ctx: &Context,
    commit: CommitId,
    sessions: &[&Session],
    all: bool,
    rerun: String,
) -> Fallible<Result<Artifact, &'static str>> {
    let spellings = root_spellings(&ctx.root);
    let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
    Ok(
        minds_reader::artifact::assess(&ctx.repo, &roots, commit, sessions)?.map(|assessed| {
            Artifact {
                recon: assessed.recon,
                structural: assessed.structural,
                all,
                rerun,
            }
        }),
    )
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
        minds_reader::artifact::summary(&self.recon)
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
fn details(files: &[FileRecon], structural: &[Structural]) -> Vec<Detail> {
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
            for range in ranges(missing) {
                unexplained.push(Detail {
                    kind: "unexplained",
                    location: format!("{path}:{}", range_text(range)),
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
                    reason: format!("{NOT_OBSERVED}; line level unavailable ({})", reason.word()),
                });
            }
            (LineLevel::Unavailable(reason), _) => file_only.push(Detail {
                kind: "file only",
                location: path,
                reason: format!("line level unavailable ({})", reason.word()),
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
    for Structural { path, what } in structural {
        unexplained.push(Detail {
            kind: "unexplained",
            location: shown_path(path),
            reason: format!("{what} {NOT_OBSERVED}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::ContentHash;
    use minds_reader::reconcile::{LineRecon, Reason};

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
        a.structural.push(Structural {
            path: "vendor/crypto".into(),
            what: "submodule pointer",
        });
        assert!(a.gate_failure(1).is_some());
        assert_eq!(
            a.detail_lines(),
            ["  unexplained    vendor/crypto  submodule pointer not observed in the session"]
        );
    }

    #[test]
    fn overlong_paths_are_cut_from_the_front() {
        let long = format!("{}/x.rs", "d".repeat(1000));
        let shown = shown_path(&long);
        assert_eq!(shown.chars().count(), PATH_CAP);
        assert!(shown.starts_with('…') && shown.ends_with("/x.rs"));
        assert_eq!(shown_path("a.rs"), "a.rs");
    }
}
