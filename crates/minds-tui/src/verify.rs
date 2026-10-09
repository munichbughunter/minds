//! Der Verify-Tab: das Urteil über einen Commit auf einen Blick — statt der
//! Textwand von `minds verify`. Zuerst Urteil, Stufe und Intent, dann je
//! Achse eine Zeile, die Artefakt-Klassen als Balken.
//!
//! Zwei Quellen, getrennt gehalten: Was der Reader allein weiß (Integrität,
//! Coverage, Epochen je Session; der Abgleich des Commits), steht sofort da.
//! Was Signaturen braucht (Assurance, Intent-Signatur, Scope), liefert die
//! [`crate::Source`] — die CLI, mit denselben Bausteinen wie `minds verify`.
//! Fehlt das, sagt der Tab es, statt zu raten.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use minds_core::SessionId;
use minds_git::{CommitId, Repo};
use minds_reader::Inspection;
use minds_reader::artifact::{ArtifactState, roots_of};
use minds_reader::model::{EvidenceReport, EvidenceVerdict, ReviewState};
use minds_reader::reconcile::{GapCounts, LineLevel, ReconClass, Reconciliation};

use crate::CommitVerify;

/// Zeilen je Klasse über alle geänderten Zeilen eines Commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassCounts {
    /// ● Witness und Claim.
    pub explained: u64,
    /// ◍ nur Witness.
    pub fs_only: u64,
    /// ◇ nur Claim.
    pub reported: u64,
    /// ◦ ohne Beleg.
    pub unexplained: u64,
    /// Die unerklärten Zeilen nach ihrem Grund.
    pub gaps: GapCounts,
}

impl ClassCounts {
    /// Alle geänderten Zeilen.
    pub fn total(&self) -> u64 {
        self.explained + self.fs_only + self.reported + self.unexplained
    }

    /// Zeilen mit irgendeinem Beleg.
    pub fn backed(&self) -> u64 {
        self.total() - self.unexplained
    }

    /// Aus einem Abgleich: je Zeile, wo der Abgleich Zeilen kennt, sonst die
    /// Klasse der Datei für alle ihre geänderten Zeilen.
    pub fn of(recon: &Reconciliation) -> Self {
        let mut counts = Self::default();
        let mut add = |class: ReconClass, n: u64| match class {
            ReconClass::Explained => counts.explained += n,
            ReconClass::ExplainedFsOnly => counts.fs_only += n,
            ReconClass::ReportedOnly => counts.reported += n,
            ReconClass::Unexplained => counts.unexplained += n,
        };
        counts.gaps = recon.unexplained_by_gap();
        for file in &recon.files {
            match &file.line_level {
                LineLevel::Available(lines) => {
                    for line in lines {
                        add(line.class, 1);
                    }
                }
                LineLevel::Unavailable(_) => add(file.class, file.changed_lines),
            }
        }
        counts
    }
}

/// Eine Session am Commit, wie der Reader sie kennt.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// Die Session.
    pub id: SessionId,
    /// Ihr Auftrag, entschärft.
    pub request: String,
    /// Ihr Evidence-Report; `None` bei Legacy oder unlesbar.
    pub report: Option<EvidenceReport>,
}

/// Das Urteil, zusammengefasst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overall {
    /// Intakt und vollständig.
    Verified,
    /// Intakt, aber lückenhaft.
    Incomplete,
    /// Eine Session ohne Seal — nichts zu verifizieren (wie `verify`).
    NotVerifiable,
    /// Die Signer werden noch geprüft — noch kein Urteil.
    Checking,
    /// Die Signer ließen sich nicht prüfen — kein VERIFIED ohne sie.
    NotChecked,
    /// Material verändert.
    Tampered,
    /// Keine Session am Commit.
    NoSession,
}

/// Der Verify-Tab.
#[derive(Debug, Clone)]
pub struct VerifyState {
    /// Die blätterbaren Commits (HEAD zuerst) — dieselben wie im Changes-Tab.
    pub commits: Vec<CommitId>,
    /// Der gezeigte, Index in `commits`.
    pub at: usize,
    /// Betreff, entschärft.
    pub subject: Option<String>,
    /// Change-Id.
    pub change: Option<String>,
    /// Review-Stand.
    pub review: ReviewState,
    /// Die Sessions am Commit.
    pub sessions: Vec<SessionRow>,
    /// Die Klassen der geänderten Zeilen — oder warum es keinen Abgleich gibt.
    pub artifact: Result<ClassCounts, String>,
    /// Der signaturabhängige Teil aus der CLI; `None`, solange (oder weil)
    /// er nicht vorliegt.
    pub cli: Option<Result<CommitVerify, String>>,
    /// Ob `cli` noch angefragt werden muss — `run` tut das, `reduce` bleibt
    /// ohne Quelle prüfbar.
    pub pending: bool,
    /// Vom Nutzer verlangt (Tab geöffnet, Commit gewechselt): sofort prüfen.
    /// Nach einem Neuladen von selbst gedrosselt.
    pub urgent: bool,
    /// Die gewählte Zeile (Sessions, dann Artefakt, dann Scope).
    pub cursor: usize,
}

impl VerifyState {
    /// Baut den Reader-Teil für `commits[at]`.
    pub fn build(inspection: &Inspection, repo: &Repo, commits: Vec<CommitId>, at: usize) -> Self {
        Self::build_with(inspection, repo, commits, at, None)
    }

    /// Wie [`build`](Self::build); mit `artifact` wird ein schon gerechneter
    /// Abgleich übernommen statt neu gelesen (kein `git diff-tree`) — der
    /// Aufrufer verantwortet, dass sich Commit und Claimants nicht geändert
    /// haben.
    pub fn build_with(
        inspection: &Inspection,
        repo: &Repo,
        commits: Vec<CommitId>,
        at: usize,
        artifact_known: Option<Result<ClassCounts, String>>,
    ) -> Self {
        let commit = commits.get(at).copied();
        let index = inspection.index();
        let mut sessions = Vec::new();
        let mut artifact = Err("no commit yet".to_string());
        let (mut subject, mut change, mut review) = (None, None, ReviewState::open());
        if let Some(commit) = commit {
            // Wie `verify`: die Sessions des Trailers, nur ohne Trailer die
            // Kanten des Store-Index.
            let trailer = index.trailer_ids(commit);
            let ids: Vec<SessionId> = if trailer.is_empty() {
                index.sessions_of(commit).to_vec()
            } else {
                trailer.to_vec()
            };
            sessions = ids
                .into_iter()
                .map(|id| SessionRow {
                    id,
                    request: inspection
                        .card(id)
                        .map(|c| c.summary.headline)
                        .unwrap_or_else(|| "(not readable)".into()),
                    report: inspection.evidence_report(id),
                })
                .collect();
            artifact = match artifact_known {
                Some(known) => known,
                None => {
                    let spellings: Vec<PathBuf> = roots_of(repo);
                    let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
                    match index.artifact(repo, &roots, commit).state {
                        ArtifactState::Assessed(assessed) => Ok(ClassCounts::of(&assessed.recon)),
                        ArtifactState::Unavailable(why) => Err(why.to_string()),
                        ArtifactState::Failed(err) => Err(err),
                    }
                }
            };
            subject = index.subject_of(commit).map(str::to_owned);
            change = index.change_of(commit).map(|c| c.to_string());
            review = inspection.review_state_of_commit(commit);
        }
        let has_sessions = !sessions.is_empty();
        Self {
            commits,
            at,
            subject,
            change,
            review,
            sessions,
            artifact,
            cli: None,
            // Ohne Session nichts zu prüfen — kein `minds verify`.
            pending: commit.is_some() && has_sessions,
            urgent: true,
            cursor: 0,
        }
    }

    /// Ob jetzt geprüft werden soll: ausstehend und entweder vom Nutzer
    /// verlangt oder die Drossel (`every`) seit der letzten Prüfung um.
    pub fn due(&self, since_last: std::time::Duration, every: std::time::Duration) -> bool {
        self.pending && (self.urgent || since_last >= every)
    }

    /// Der gezeigte Commit.
    pub fn commit(&self) -> Option<CommitId> {
        self.commits.get(self.at).copied()
    }

    /// Wie viele wählbare Zeilen: je Session eine, dann Artefakt und Scope.
    pub fn rows(&self) -> usize {
        self.sessions.len() + 2
    }

    /// Die Zeile des Artefakts.
    pub fn artifact_row(&self) -> usize {
        self.sessions.len()
    }

    /// Die Zeile des Scopes.
    pub fn scope_row(&self) -> usize {
        self.sessions.len() + 1
    }

    /// Der geprüfte Teil aus der CLI — nur, wenn die Prüfung durchlief **und**
    /// genau die gezeigten Sessions geprüft hat.
    pub fn checked(&self) -> Option<&CommitVerify> {
        let cli = self.cli.as_ref()?.as_ref().ok()?;
        let shown: BTreeSet<SessionId> = self.sessions.iter().map(|s| s.id).collect();
        let checked: BTreeSet<SessionId> = cli.sessions.iter().map(|s| s.session).collect();
        (shown == checked).then_some(cli)
    }

    /// Warum die Signer nicht geprüft sind, falls nicht — dann gilt nichts
    /// aus der CLI (Stufe, Intent, Scope).
    pub fn check_error(&self) -> Option<String> {
        match self.cli.as_ref()? {
            Err(err) => Some(err.clone()),
            Ok(_) if self.checked().is_none() => {
                Some("the checked sessions differ from the shown ones".into())
            }
            Ok(_) => None,
        }
    }

    /// Warum es kein Urteil von `minds verify` gibt, obwohl die Signer
    /// geprüft sind (Binary im Checkout, ausgetauscht, Zeitlimit).
    pub fn verdict_error(&self) -> Option<String> {
        self.checked()?.verdict.as_ref().err().cloned()
    }

    /// Ob der Reader allein schon Manipulation sieht (Seal-Hash falsch) —
    /// das sagt `verify` dann ebenfalls.
    fn reader_tampered(&self) -> bool {
        self.sessions.iter().any(|s| {
            s.report
                .as_ref()
                .is_some_and(|r| r.state.verdict == EvidenceVerdict::Tampered)
        })
    }

    /// Das Urteil — das von `minds verify` selbst (Exit-Code), nichts
    /// nachgebaut. Ohne dieses Urteil höchstens TAMPERED (das sieht der
    /// Reader am Hash), sonst CHECKING oder NOT CHECKED: nie VERIFIED,
    /// nie INCOMPLETE.
    pub fn overall(&self) -> Overall {
        if self.sessions.is_empty() {
            return Overall::NoSession;
        }
        let tampered_cli = self
            .checked()
            .is_some_and(|c| c.sessions.iter().any(|s| s.tampered));
        if self.reader_tampered() || tampered_cli {
            return Overall::Tampered;
        }
        match self.checked().map(|c| &c.verdict) {
            Some(Ok(crate::VerifyVerdict::Verified)) => Overall::Verified,
            Some(Ok(crate::VerifyVerdict::Incomplete)) => Overall::Incomplete,
            Some(Ok(crate::VerifyVerdict::Tampered)) => Overall::Tampered,
            Some(Ok(crate::VerifyVerdict::NotVerifiable)) => Overall::NotVerifiable,
            _ if self.pending => Overall::Checking,
            _ => Overall::NotChecked,
        }
    }

    /// Die Assurance einer Session aus der CLI, falls geprüft.
    pub fn assurance_of(&self, id: SessionId) -> Option<&crate::SessionAssurance> {
        self.checked()?.sessions.iter().find(|s| s.session == id)
    }

    /// Die schwächste Stufe über alle Sessions — aus der CLI (gegen die
    /// Signer geprüft), sonst aus dem Reader (höchstens A1).
    pub fn weakest(&self) -> Option<(minds_reader::assurance::Assurance, bool)> {
        if let Some(cli) = self.checked()
            && let Some(level) = cli.sessions.iter().map(|s| s.level).min()
        {
            return Some((level, true));
        }
        self.sessions
            .iter()
            .filter_map(|s| s.report.as_ref().map(|r| r.assurance))
            .min()
            .map(|level| (level, false))
    }
}
