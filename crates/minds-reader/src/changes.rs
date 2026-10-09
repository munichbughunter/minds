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

use minds_core::{ChangeId, ContentHash, Role, SessionId};
use minds_git::{CommitId, DiffKind, Repo};

use crate::artifact::{ArtifactState, roots_of};
use crate::model::ReviewState;
use crate::reconcile::{FileRecon, LINE_LEVEL_LIMIT, LineLevel, LineSource, ReconClass};
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
    /// Der gebundene Intent-Anker der Session, falls einer galt.
    pub anchor: Option<ContentHash>,
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
            anchor: session.intent_anchor.clone(),
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
