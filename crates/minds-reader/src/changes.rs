//! Die Änderungen eines Commits, wie ein Entwickler sie kennt — als
//! Unified-Diff je Datei — und dazu, je hinzugefügter Zeile, was Minds über
//! sie weiß: die Klasse aus dem Abgleich ([`crate::reconcile`]) und den
//! Schreibvorgang, der sie einführte ([`LineSource`]).
//!
//! Strikt lesend. Der Diff wird in-process aus den Blobs gerechnet
//! ([`minds_git::unified_diff`]), mit demselben Algorithmus wie der
//! Abgleich — Diff und Klassen meinen garantiert dieselben Zeilen. Der
//! einzige Unterprozess ist die gehärtete Liste der geänderten Dateien
//! ([`minds_git::Repo::commit_changes`]), dieselbe wie beim Abgleich.
//!
//! Alle Texte hier sind **Anzeige**: Pfade und Zeilen sind entschärft
//! ([`crate::sanitize_path`]), Tabulatoren zu Leerzeichen erweitert.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use minds_core::{ChangeId, Role, SessionId};
use minds_git::{CommitId, DiffKind, Repo};

use crate::artifact::{ArtifactState, roots_of};
use crate::model::ReviewState;
use crate::reconcile::{FileRecon, Gap, LINE_LEVEL_LIMIT, LineLevel, LineSource, ReconClass};
use crate::text::{sanitize, sanitize_path};

/// Kontextzeilen je Seite eines Hunks — wie `git diff`.
pub const CONTEXT: u32 = 3;

/// Höchstens so viele Dateien zeigt ein Commit mit Zeilen; der Rest steht
/// mit Namen und ohne Diff da, nie verschwiegen.
pub const MAX_FILES: usize = 300;

/// So viele Diff-Zeilen zeigt ein Commit höchstens, über alle Dateien — der
/// Rest steht mit Namen da. Begrenzt Speicher und Zeichenaufwand gegen einen
/// Commit, der viele große Dateien umschreibt.
pub const MAX_ROWS: usize = 200_000;

/// So viele Zeichen trägt eine Diff-Zeile höchstens (vor dem Entschärfen).
const ROW_CHARS: usize = 2_000;

/// So viele Zeichen trägt die Begründung höchstens.
const SAID_LIMIT: usize = 1200;

/// Eine Zeile im Diff einer Datei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// Kontext, `+`, `-` oder Hunk-Kopf.
    pub kind: DiffKind,
    /// Zeilennummer in der alten Fassung (Kontext, `-`).
    pub old: Option<u32>,
    /// Zeilennummer in der neuen Fassung (Kontext, `+`).
    pub new: Option<u32>,
    /// Der Text, entschärft, Tabs als Leerzeichen.
    pub text: String,
    /// Bei `+`: die Klasse aus dem Abgleich — `None`, wo kein Abgleich
    /// vorliegt.
    pub class: Option<ReconClass>,
    /// Bei `+`: der Schreibvorgang, der die Zeile einführte.
    pub source: Option<LineSource>,
}

/// Der Diff einer Datei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Repo-relativer Pfad, entschärft.
    pub path: String,
    /// Der Pfad als Identität (für die Why-Kette einer Zeile), unentschärft.
    pub identity: String,
    /// Hinzugefügte und entfernte Zeilen.
    pub added: usize,
    pub removed: usize,
    /// Die Klasse der Datei aus dem Abgleich.
    pub class: Option<ReconClass>,
    /// Warum ihre unerklärten Zeilen unerklärt sind.
    pub gap: Option<Gap>,
    /// Warum keine Zeilen da sind: Binärdatei, zu groß, Submodul, …
    pub note: Option<String>,
    /// Die Zeilen, in Diff-Reihenfolge.
    pub rows: Vec<DiffRow>,
}

impl FileDiff {
    /// Wie viele `+`-Zeilen nicht erklärt sind.
    pub fn unexplained(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| r.kind == DiffKind::Added && r.class == Some(ReconClass::Unexplained))
            .count()
    }
}

/// Die Änderungen eines Commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// Der Commit.
    pub commit: CommitId,
    /// Sein Betreff, entschärft.
    pub subject: Option<String>,
    /// Seine Change-Id.
    pub change: Option<ChangeId>,
    /// Der Review-Stand.
    pub review: ReviewState,
    /// Warum kein Abgleich vorliegt (Shallow/Partial Clone, Lesefehler) —
    /// der Diff steht trotzdem da, nur ohne Klassen.
    pub unassessed: Option<String>,
    /// Die Dateien, nach Pfad.
    pub files: Vec<FileDiff>,
}

/// Ein Grund, warum hinzugefügte Zeilen unerklärt sind, mit ihrer Zahl
/// (C3.6). Der Grund ist das Wort von [`Gap::word`] — dasselbe, das der
/// Changes-Tab je Zeile und `minds verify` je Commit nennen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonCount {
    /// Der Grund; [`NO_REASON`], wo der Abgleich keinen nennt.
    pub reason: &'static str,
    /// So viele unerklärte Zeilen tragen ihn.
    pub lines: usize,
}

/// Der Grund einer unerklärten Zeile, deren Datei der Abgleich keinen
/// Grund gibt — gezählt, nie verschwiegen.
pub const NO_REASON: &str = "no reason recorded by the reconciliation";

/// Die hinzugefügten Zeilen eines Commits nach Klasse.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineClasses {
    /// Alle hinzugefügten Zeilen.
    pub added: usize,
    /// Beobachtet und vom Tool beansprucht (●).
    pub explained: usize,
    /// Nur im Dateisystem beobachtet (◍).
    pub fs_only: usize,
    /// Nur ausgesagt, ohne Zeugen (◇).
    pub stated: usize,
    /// Unerklärt (◦) — wie [`FileDiff::unexplained`].
    pub unexplained: usize,
    /// Nicht bewertet: ohne Klasse oder in Dateien ohne Diff.
    pub not_assessed: usize,
    /// Dateien ohne Diff (binär, zu groß, …).
    pub skipped_files: usize,
    /// Alle Dateien, die der Commit ändert — mit und ohne Diff (C3.6: die
    /// Oberfläche stellt sie den Dateiclaims der Session gegenüber).
    pub files: usize,
    /// Warum die unerklärten Zeilen unerklärt sind (C3.6): je Grund die
    /// Zahl der Zeilen, häufigster zuerst; die Summe ist `unexplained`.
    /// Leer, wenn nichts unerklärt ist.
    pub reasons: Vec<ReasonCount>,
}

impl LineClasses {
    /// Ob jede hinzugefügte Zeile beobachtet ist — nur dann ✓.
    pub fn all_explained(&self) -> bool {
        self.added > 0 && self.explained == self.added && self.skipped_files == 0
    }
}

impl ChangeSet {
    /// Wie viele `+`-Zeilen des Commits nicht erklärt sind — die Summe über
    /// [`FileDiff::unexplained`], dieselbe Zahl wie `◦n` je Datei (C3.4).
    pub fn unexplained(&self) -> usize {
        self.files.iter().map(FileDiff::unexplained).sum()
    }

    /// Die hinzugefügten Zeilen nach Klasse (C3.4): Nur beobachtete gelten
    /// als erklärt; ausgesagte, nur im Dateisystem gesehene, unerklärte und
    /// nicht bewertete (ohne Klasse oder in Dateien ohne Diff) getrennt.
    pub fn line_classes(&self) -> LineClasses {
        let mut c = LineClasses {
            added: self.added(),
            unexplained: self.unexplained(),
            files: self.files.len(),
            reasons: self.unexplained_reasons(),
            ..LineClasses::default()
        };
        for file in &self.files {
            if file.note.is_some() {
                c.skipped_files += 1;
            }
            for row in file.rows.iter().filter(|r| r.kind == DiffKind::Added) {
                match row.class {
                    Some(ReconClass::Explained) => c.explained += 1,
                    Some(ReconClass::ExplainedFsOnly) => c.fs_only += 1,
                    Some(ReconClass::ReportedOnly) => c.stated += 1,
                    Some(ReconClass::Unexplained) | None => {}
                }
            }
        }
        let classified = c.explained + c.fs_only + c.stated + c.unexplained;
        c.not_assessed = c.added.saturating_sub(classified);
        c
    }

    /// Warum die unerklärten Zeilen unerklärt sind (C3.6): je unerklärter
    /// Zeile der Grund ihrer Datei ([`FileDiff::gap`]), nach Wort gruppiert,
    /// häufigster zuerst (bei Gleichstand alphabetisch). Dieselben Zeilen
    /// wie [`ChangeSet::unexplained`] — eine Quelle für Overview- und
    /// Changes-Tab.
    pub fn unexplained_reasons(&self) -> Vec<ReasonCount> {
        let mut by_word: BTreeMap<&'static str, usize> = BTreeMap::new();
        for file in &self.files {
            let lines = file.unexplained();
            if lines == 0 {
                continue;
            }
            let word = file.gap.map(Gap::word).unwrap_or(NO_REASON);
            *by_word.entry(word).or_default() += lines;
        }
        let mut reasons: Vec<ReasonCount> = by_word
            .into_iter()
            .map(|(reason, lines)| ReasonCount { reason, lines })
            .collect();
        reasons.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.reason.cmp(b.reason)));
        reasons
    }

    /// Wie viele Zeilen der Commit hinzufügt.
    pub fn added(&self) -> usize {
        self.files.iter().map(|f| f.added).sum()
    }
}

/// Warum eine Zeile existiert — aus der Session, die sie schrieb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineReason {
    /// Die Session.
    pub session: SessionId,
    /// Agent und Modell, entschärft.
    pub agent: String,
    /// Der Auftrag der Session (erfasster Prompt), entschärft, einzeilig.
    pub request: String,
    /// Der Turn und der Aufruf.
    pub turn: usize,
    pub call: usize,
    /// Das Werkzeug (`Write`, `Edit`, …), entschärft.
    pub tool: String,
    /// Wann der Turn war.
    pub at: Option<String>,
    /// Was der Agent zu diesem Schritt schrieb — der Text des Turns oder,
    /// ist der leer, des letzten Assistenten-Turns davor; entschärft,
    /// einzeilig, gekürzt.
    pub said: Option<String>,
    /// Aus welchem Turn `said` stammt — ein früherer als `turn`, wenn der
    /// Turn des Aufrufs selbst keinen Text trägt.
    pub said_turn: Option<usize>,
    /// Der Anker, an den die Session sich gebunden nennt (Record,
    /// ungeprüft) — Witness-Event vor lokaler Datei.
    pub anchor: Option<crate::intent::BoundAnchor>,
}

/// Eine Zeile für die Anzeige: Tabs als vier Leerzeichen, dann entschärft.
fn display(text: &str) -> String {
    let clip = |s: &str| -> String {
        let clipped: String = s.chars().take(ROW_CHARS).collect();
        if clipped.len() < s.len() {
            format!("{clipped}…")
        } else {
            clipped
        }
    };
    // Vor und nach dem Entschärfen gekürzt: Steuerzeichen werden dabei bis
    // zu zehnmal länger (`\u{1}`), das Ergebnis bleibt trotzdem begrenzt.
    clip(&sanitize_path(&clip(text).replace('\t', "    ")))
}

/// Ein Fließtext in einer Zeile: Umbrüche als Leerzeichen, entschärft,
/// gekürzt.
fn one_line(text: &str, limit: usize) -> String {
    // Erst grob kürzen: Ein Turn-Text kann Megabytes groß sein, und das
    // geschieht je gezeichnetem Frame.
    let head: String = text.chars().take(limit.saturating_mul(4)).collect();
    let flat: String = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = flat.chars().take(limit).collect();
    let clipped = if flat.chars().count() > limit {
        format!("{clipped}…")
    } else {
        clipped
    };
    sanitize(&clipped)
}

impl crate::Inspection {
    /// Die Commits, deren Änderungen sich ansehen lassen: HEAD zuerst, dann
    /// jeder Commit der HEAD-Historie, den eine Session trägt, in
    /// Revwalk-Reihenfolge. Commits außerhalb von HEAD (etwa durch ein Amend
    /// ersetzte) fehlen.
    pub fn change_commits(&self) -> Vec<CommitId> {
        let index = self.index();
        let mut linked: Vec<CommitId> = index
            .sessions()
            .flat_map(|(id, _)| index.commits_of(*id))
            .filter(|commit| index.position(*commit).is_some())
            .collect();
        linked.sort_by_key(|commit| index.position(*commit));
        linked.dedup();
        let mut out: Vec<CommitId> = self.head().into_iter().collect();
        out.extend(linked.into_iter().filter(|c| Some(*c) != self.head()));
        out
    }

    /// Die Änderungen von `commit` gegen den ersten Elternteil: der Diff je
    /// Datei und, aus dem Abgleich, Klasse und Herkunft je `+`-Zeile.
    ///
    /// Fail-soft beim Abgleich (dann `unassessed`); ein Fehler beim Lesen
    /// des Diffs selbst ist `Err`.
    pub fn changes(&self, repo: &Repo, commit: CommitId) -> minds_git::Result<ChangeSet> {
        let spellings: Vec<PathBuf> = roots_of(repo);
        let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
        let artifact = self.index().artifact_detailed(repo, &roots, commit, true);
        let (recon, unassessed): (BTreeMap<String, FileRecon>, Option<String>) =
            match artifact.state {
                ArtifactState::Assessed(assessed) => (
                    assessed
                        .recon
                        .files
                        .into_iter()
                        .map(|f| (f.path.clone(), f))
                        .collect(),
                    None,
                ),
                ArtifactState::Unavailable(why) => (BTreeMap::new(), Some(why.to_string())),
                ArtifactState::Failed(err) => (BTreeMap::new(), Some(err)),
            };
        let changed = repo.commit_changes(commit)?;
        let mut files = Vec::new();
        let mut rows = 0usize;
        for (i, entry) in changed.files.iter().enumerate() {
            let recon = recon.get(&entry.path);
            let mut file = FileDiff {
                path: display(&entry.path),
                identity: entry.path.clone(),
                added: 0,
                removed: 0,
                class: recon.map(|r| r.class),
                gap: recon.and_then(|r| r.gap),
                note: None,
                rows: Vec::new(),
            };
            let read = |id: Option<minds_git::BlobId>| -> minds_git::Result<Vec<u8>> {
                id.map(|id| repo.read_blob_id(id))
                    .transpose()
                    .map(Option::unwrap_or_default)
            };
            if i >= MAX_FILES {
                file.note = Some("not shown: too many files in this commit".into());
            } else if rows >= MAX_ROWS {
                file.note = Some("not shown: this commit is too large to show in full".into());
            } else if entry.gitlink {
                file.note = Some("submodule pointer".into());
            } else if entry.symlink {
                file.note = Some("symlink".into());
            } else if let (Ok(base), Ok(committed)) = (read(entry.base), read(entry.committed)) {
                if base.contains(&0) || committed.contains(&0) {
                    file.note = Some("binary".into());
                } else if base.len().max(committed.len()) > LINE_LEVEL_LIMIT {
                    file.note = Some("too large to show".into());
                } else {
                    let lines = lines_of(recon);
                    let diff = minds_git::unified_diff(&base, &committed, CONTEXT);
                    // Das Budget gilt dem ganzen Commit: Eine Datei, die es
                    // sprengen würde, steht mit Namen da, nicht halb.
                    if rows + diff.len() > MAX_ROWS {
                        // Nur diese Datei; kleinere danach passen noch.
                        file.note =
                            Some("not shown: this commit is too large to show in full".into());
                        files.push(file);
                        continue;
                    }
                    rows += diff.len();
                    for line in diff {
                        let (class, source) = match (line.kind, line.new) {
                            (DiffKind::Added, Some(n)) => match &lines {
                                Lines::Each(map) => map.get(&n).copied().unwrap_or((None, None)),
                                Lines::All(class) => (*class, None),
                            },
                            _ => (None, None),
                        };
                        match line.kind {
                            DiffKind::Added => file.added += 1,
                            DiffKind::Removed => file.removed += 1,
                            _ => {}
                        }
                        file.rows.push(DiffRow {
                            kind: line.kind,
                            old: line.old,
                            new: line.new,
                            text: display(&line.text),
                            class,
                            source,
                        });
                    }
                }
            } else {
                // Ein fehlender Blob (Partial Clone) kostet nur diese Datei,
                // nicht den Tab.
                file.note = Some("blob not available in this clone".into());
            }
            files.push(file);
        }
        let index = self.index();
        Ok(ChangeSet {
            commit,
            subject: index.subject_of(commit).map(str::to_owned),
            change: index.change_of(commit).cloned(),
            review: self.review_state_of_commit(commit),
            unassessed,
            files,
        })
    }

    /// Warum eine Zeile existiert: was die Session, die sie schrieb, zu
    /// diesem Schritt festhielt. `None`, wenn die Session (vergessen,
    /// unlesbar) oder der Aufruf nicht da ist.
    pub fn line_reason(&self, source: LineSource) -> Option<LineReason> {
        let session = self.index().session(source.session)?;
        let turn = session.turns.get(source.turn)?;
        let call = turn.tool_calls.get(source.call)?;
        let said = (0..=source.turn)
            .rev()
            .map(|i| (i, &session.turns[i]))
            .filter(|(_, t)| t.role == Role::Assistant)
            .map(|(i, t)| (i, t.text.trim()))
            .find(|(_, text)| !text.is_empty())
            .map(|(i, text)| (i, one_line(text, SAID_LIMIT)));
        let (said_turn, said) = match said {
            Some((i, text)) => (Some(i), Some(text)),
            None => (None, None),
        };
        Some(LineReason {
            session: source.session,
            agent: one_line(
                &format!("{} · {}", session.agent.name, session.model.id),
                80,
            ),
            request: one_line(&session.intent.request, SAID_LIMIT),
            turn: source.turn,
            call: source.call,
            tool: one_line(&call.name, 40),
            at: turn.at.as_deref().map(|at| one_line(at, 40)),
            said,
            said_turn,
            anchor: crate::intent::bound_anchor(session),
        })
    }
}

/// Die Klassen einer Datei je Zeile — oder eine für alle, wo der Abgleich
/// nur die Datei kennt.
enum Lines {
    Each(BTreeMap<u32, (Option<ReconClass>, Option<LineSource>)>),
    All(Option<ReconClass>),
}

fn lines_of(recon: Option<&FileRecon>) -> Lines {
    match recon.map(|r| (&r.line_level, r.class)) {
        Some((LineLevel::Available(lines), _)) => Lines::Each(
            lines
                .iter()
                .map(|l| (l.line, (Some(l.class), l.source.as_deref().copied())))
                .collect(),
        ),
        Some((LineLevel::Unavailable(_), class)) => Lines::All(Some(class)),
        None => Lines::All(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(class: Option<ReconClass>) -> DiffRow {
        DiffRow {
            kind: DiffKind::Added,
            old: None,
            new: Some(1),
            text: String::new(),
            class,
            source: None,
        }
    }

    fn file(added: usize, rows: Vec<DiffRow>) -> FileDiff {
        FileDiff {
            path: "f".into(),
            identity: "f".into(),
            added,
            removed: 0,
            class: None,
            gap: None,
            note: None,
            rows,
        }
    }

    /// C3.4: Die Zahl unerklärter Zeilen eines Commits ist die Summe der
    /// Zahlen je Datei (`◦n` im Changes-Tab) — eine Funktion, keine zweite
    /// Rechnung.
    #[test]
    fn the_commit_total_is_the_sum_of_the_files() {
        let set = ChangeSet {
            commit: format!("{:040x}", 1).parse().unwrap(),
            subject: None,
            change: None,
            review: ReviewState::default(),
            unassessed: None,
            files: vec![
                file(
                    3,
                    vec![
                        row(Some(ReconClass::Unexplained)),
                        row(Some(ReconClass::Unexplained)),
                        row(None),
                    ],
                ),
                file(2, vec![row(Some(ReconClass::Unexplained))]),
            ],
        };
        let per_file: usize = set.files.iter().map(FileDiff::unexplained).sum();
        assert_eq!(set.unexplained(), per_file);
        assert_eq!(set.unexplained(), 3);
        assert_eq!(set.added(), 5);
    }

    /// C3.6: Die Gründe je Commit kommen aus denselben Zeilen wie `◦n`:
    /// je unerklärter Zeile der Grund ihrer Datei, gruppiert nach Wort,
    /// häufigster zuerst — die Summe ist die Zahl der unerklärten Zeilen.
    /// Dateien ohne Grund zählen als „kein Grund", nie stillschweigend.
    #[test]
    fn unexplained_reasons_group_by_word_and_sum_to_the_count() {
        let mut untouched = file(
            3,
            vec![
                row(Some(ReconClass::Unexplained)),
                row(Some(ReconClass::Unexplained)),
                row(Some(ReconClass::Explained)),
            ],
        );
        untouched.gap = Some(Gap::Untouched { complete: true });
        let mut unhashed = file(1, vec![row(Some(ReconClass::Unexplained))]);
        unhashed.gap = Some(Gap::Unhashed);
        let mut also_untouched = file(1, vec![row(Some(ReconClass::Unexplained))]);
        also_untouched.gap = Some(Gap::Untouched { complete: true });
        let silent = file(1, vec![row(Some(ReconClass::Unexplained))]);
        let set = ChangeSet {
            commit: format!("{:040x}", 1).parse().unwrap(),
            subject: None,
            change: None,
            review: ReviewState::default(),
            unassessed: None,
            files: vec![untouched, unhashed, also_untouched, silent],
        };
        let c = set.line_classes();
        assert_eq!(c.files, 4, "every file of the commit, diffed or not");
        assert_eq!(
            c.reasons,
            vec![
                ReasonCount {
                    reason: Gap::Untouched { complete: true }.word(),
                    lines: 3,
                },
                // Gleichstand: alphabetisch — „a tool claim …" vor „no reason …".
                ReasonCount {
                    reason: Gap::Unhashed.word(),
                    lines: 1,
                },
                ReasonCount {
                    reason: NO_REASON,
                    lines: 1,
                },
            ]
        );
        let sum: usize = c.reasons.iter().map(|r| r.lines).sum();
        assert_eq!(sum, c.unexplained);
        assert_eq!(sum, set.unexplained());
        // Ohne unerklärte Zeilen keine Gründe — „leer ist leer".
        let clean = ChangeSet {
            files: vec![file(1, vec![row(Some(ReconClass::Explained))])],
            ..set
        };
        assert!(clean.line_classes().reasons.is_empty());
    }

    /// Review C3.4: Nur beobachtete Zeilen sind „erklärt" — ausgesagte
    /// (ReportedOnly), unklassifizierte und nicht gediffte zählen getrennt,
    /// nie als erklärt.
    #[test]
    fn line_classes_never_count_a_claim_as_explained() {
        let mut skipped = file(7, Vec::new());
        skipped.note = Some("binary".into());
        let set = ChangeSet {
            commit: format!("{:040x}", 1).parse().unwrap(),
            subject: None,
            change: None,
            review: ReviewState::default(),
            unassessed: None,
            files: vec![
                file(
                    5,
                    vec![
                        row(Some(ReconClass::Explained)),
                        row(Some(ReconClass::ExplainedFsOnly)),
                        row(Some(ReconClass::ReportedOnly)),
                        row(Some(ReconClass::Unexplained)),
                        row(None),
                    ],
                ),
                skipped,
            ],
        };
        let c = set.line_classes();
        assert_eq!(c.added, 12);
        assert_eq!(c.explained, 1);
        assert_eq!(c.fs_only, 1);
        assert_eq!(c.stated, 1);
        assert_eq!(c.unexplained, set.unexplained());
        assert_eq!(
            c.not_assessed, 8,
            "one unclassified row and seven in the binary"
        );
        assert_eq!(c.skipped_files, 1);
        assert!(!c.all_explained());
        let clean = ChangeSet {
            files: vec![file(1, vec![row(Some(ReconClass::Explained))])],
            ..set
        };
        assert!(clean.line_classes().all_explained());
    }
}
