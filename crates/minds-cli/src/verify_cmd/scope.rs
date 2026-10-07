//! Scope-Befunde für `minds verify` (EA-17): Blieb die Arbeit im Bereich,
//! den der Intent-Anker erklärt?
//!
//! Die Ableitung ist [`minds_reader::scope`] (rein, ohne I/O); hier werden
//! die Quellen eingesammelt — die geänderten Pfade des abgeglichenen
//! Commits, die Claims der Session, die vertrauenswürdigen Beobachtungen in
//! ihrem Fenster — und das Ergebnis gesetzt:
//!
//! ```text
//! Coverage       complete within the boundary (… · artifact 2/2 lines explained · 1 out of scope)
//!   out of scope   docs/README.md  (commit, claim)
//! ```
//!
//! Ohne erklärten Bereich (nicht gebunden, `scope=-`) fehlen Segment und
//! Detailzeilen; ein gebundener, aber nicht lesbarer Anker steht als
//! `scope not assessed (…)` im Segment. Befunde ändern nie Verdikt oder
//! Exit-Code (W6); `--require-in-scope` ist das Gate (Exit 2, maskiert nie
//! 1/3/4) und urteilt fail-closed: Nicht beurteilbar heißt nicht bestanden.
//!
//! Pfade sind fremdbestimmt und laufen vor dem Druck durch
//! [`super::artifact::shown_path`].

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use minds_core::{EffectKind, Session, SessionId};
use minds_git::{GitError, Repo, TreeId};
use minds_reader::assurance::IntentState;
use minds_reader::observations::WindowObservations;
use minds_reader::reconcile::{Claims, claim_path};
use minds_reader::scope::{Finding, NoScope, ScopeFinding, declared_scope, scope_findings};
use minds_store::ContextStore;

use super::ArtifactState;
use super::artifact::{DETAIL_CAP, KIND_WIDTH, shown_path};

/// Was über den Bereich einer Session sagbar ist.
pub(super) enum ScopeState {
    /// Ein Bereich ist erklärt; die Pfade außerhalb davon.
    Assessed(Vec<ScopeFinding>),
    /// Kein Bereich lesbar — warum.
    NotAssessed(NoScope),
}

/// Die Scope-Befunde der Session `session` unter ihrer Intent-Lage
/// `intent`. `observations` sind schon auf das Fenster der Session und auf
/// Witness-signierte Seals beschränkt; fehlt dabei ein bezeugtes Objekt,
/// ist der Bereich nicht beurteilbar (fail-closed). Strikt lesend.
pub(super) fn assess(
    store: &dyn ContextStore,
    repo: &Repo,
    root: &Path,
    intent: &IntentState,
    session: Option<&Session>,
    artifact: &ArtifactState,
    observations: &WindowObservations,
) -> ScopeState {
    if session.is_none() {
        return ScopeState::NotAssessed(NoScope::PayloadUnreadable);
    }
    let scope = match declared_scope(store, intent) {
        Ok(scope) => scope,
        Err(why) => return ScopeState::NotAssessed(why),
    };
    if !observations.complete {
        return ScopeState::NotAssessed(NoScope::ObservationsIncomplete);
    }
    let observations = observations.observations.as_slice();
    let changed: Vec<&str> = match artifact {
        ArtifactState::Assessed(artifact) => artifact.changed_paths().collect(),
        _ => Vec::new(),
    };
    // Kandidaten des Erfassungszeit-Fallbacks (`claim_path`) bestätigt ein
    // Pfad im Basis- oder Commit-Baum oder unter den geänderten Pfaden —
    // die Regel des Artefakt-Abgleichs, damit ein Claim in CI (anderer
    // Checkout-Pfad) nicht anders zählt als lokal —, dazu ein Pfad, den der
    // Witness beobachtet hat (geschrieben, vor dem Commit wieder gelöscht).
    let named: BTreeSet<&str> = changed
        .iter()
        .copied()
        .chain(observations.iter().map(|o| o.path.as_str()))
        .collect();
    let trees: Vec<TreeId> = match artifact {
        ArtifactState::Assessed(artifact) => {
            let (commit, base) = artifact.commits();
            match [Some(commit), base]
                .into_iter()
                .flatten()
                .map(|c| repo.tree_of(c))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(trees) => trees,
                Err(_) => return ScopeState::NotAssessed(NoScope::ClaimsUnresolvable),
            }
        }
        _ => Vec::new(),
    };
    let lookup_failed = Cell::new(false);
    // Jeder Kandidat nur einmal — lange Sessions fragen dieselben tausendfach.
    let seen = RefCell::new(HashMap::<String, bool>::new());
    let known = |path: &str| {
        if named.contains(path) {
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
                Err(_) => {
                    lookup_failed.set(true);
                    false
                }
            });
        seen.borrow_mut().insert(path.to_owned(), found);
        found
    };
    let spellings = minds_reader::artifact::root_spellings(root);
    let roots: Vec<&Path> = spellings.iter().map(PathBuf::as_path).collect();
    let sessions: Vec<&Session> = session.into_iter().collect();
    let claims = Claims::collect(&sessions, &roots, &known);
    // Ein Claim unterhalb der Wurzel, der sich keinem Pfad zuordnen lässt
    // (`src/../.github/…`, eine mehrdeutige Schreibweise), fiele sonst still
    // aus der Prüfung: nicht beurteilbar statt „im Bereich". Nur mit
    // abgeglichenem Commit — ohne seine Bäume trägt der Fallback nicht, und
    // das Gate fällt dann ohnehin mit dem genaueren Grund des Commits.
    let unresolved = matches!(artifact, ArtifactState::Assessed(_))
        && sessions
            .iter()
            .any(|session| unresolved_claim(session, &roots, &known));
    if lookup_failed.get() || unresolved {
        return ScopeState::NotAssessed(NoScope::ClaimsUnresolvable);
    }
    ScopeState::Assessed(scope_findings(&scope, changed, &claims, observations))
}

/// Ob ein Schreib- oder Lösch-Claim der Session unterhalb der Wurzel liegt
/// (relativ, oder absolut unter einer Wurzel-Schreibweise bzw. dem `cwd`),
/// aber keinem Repository-Pfad zuzuordnen ist. Claims außerhalb des
/// Checkouts sind keine Scope-Frage.
fn unresolved_claim(session: &Session, roots: &[&Path], known: &dyn Fn(&str) -> bool) -> bool {
    let cwd = session.lineage.as_ref().and_then(|l| l.cwd.as_deref());
    let below = |path: &str| {
        let candidate = Path::new(path);
        if path.starts_with('~') || path.starts_with('$') {
            return false;
        }
        candidate.is_relative()
            || roots.iter().any(|root| candidate.starts_with(root))
            || cwd
                .map(Path::new)
                .is_some_and(|cwd| cwd.is_absolute() && candidate.starts_with(cwd))
    };
    session
        .turns
        .iter()
        .flat_map(|turn| turn.tool_calls.iter())
        .filter_map(|call| call.effect.as_ref())
        .filter(|effect| matches!(effect.kind, EffectKind::Write | EffectKind::Delete))
        .filter_map(|effect| Some((effect.path.as_deref()?, effect.kind)))
        .any(|(path, kind)| {
            below(path) && claim_path(path, cwd, roots, known, kind == EffectKind::Delete).is_none()
        })
}

/// Das Segment für die Coverage-Zeile (`1 out of scope`). Ohne erklärten
/// Bereich (nicht gebunden, `scope=-`) keines; ist der gebundene Anker nicht
/// lesbar, sagt das Segment es — sonst sähe ein veränderter Anker aus wie
/// einer ohne Bereich.
pub(super) fn segment(state: &ScopeState) -> Option<String> {
    match state {
        ScopeState::Assessed(findings) => {
            Some(format!("{} {}", findings.len(), Finding::OutOfScope.word()))
        }
        ScopeState::NotAssessed(
            why @ (NoScope::AnchorMissing
            | NoScope::AnchorUnreadable
            | NoScope::ObservationsIncomplete
            | NoScope::IntentChangedMidSession
            | NoScope::UnsupportedGlob
            | NoScope::ClaimsUnresolvable),
        ) => Some(format!("scope not assessed ({})", why.word())),
        // Eine vergessene Nutzlast sagt nichts über Bindung — kein Segment
        // in jeder Session, die nie einen Bereich hatte; nur das Gate nennt
        // den Grund.
        ScopeState::NotAssessed(
            NoScope::IntentNotBound
            | NoScope::NotDeclared
            | NoScope::IntegrityViolated
            | NoScope::PayloadUnreadable
            | NoScope::NoEvidenceChain,
        ) => None,
    }
}

/// Die Detailzeilen, gekappt auf [`DETAIL_CAP`] (außer mit `all`); `rerun`
/// ist der Aufruf, der sie ungekappt wiederholt.
pub(super) fn detail_lines(state: &ScopeState, all: bool, rerun: &str) -> Vec<String> {
    let ScopeState::Assessed(findings) = state else {
        return Vec::new();
    };
    let shown = if all {
        &findings[..]
    } else {
        &findings[..findings.len().min(DETAIL_CAP)]
    };
    let rows: Vec<(String, String)> = shown
        .iter()
        .map(|finding| {
            let sources: Vec<&str> = finding.sources.iter().map(|s| s.word()).collect();
            (shown_path(&finding.path), sources.join(", "))
        })
        .collect();
    let width = rows
        .iter()
        .map(|(path, _)| path.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = rows
        .iter()
        .map(|(path, sources)| {
            format!(
                "  {:<KIND_WIDTH$}{path:<width$}  ({sources})",
                Finding::OutOfScope.word()
            )
        })
        .collect();
    if shown.len() < findings.len() {
        lines.push(format!(
            "  … {} more ({rerun})",
            findings.len() - shown.len()
        ));
    }
    lines
}

/// Der Aufruf, der die Detailzeilen ungekappt wiederholt: der des
/// Artefakt-Abgleichs, ohne Commit die Session selbst.
pub(super) fn rerun(artifact: &ArtifactState, id: SessionId) -> String {
    match artifact {
        ArtifactState::Assessed(artifact) => artifact.rerun().to_owned(),
        _ => format!("minds verify {id} --all"),
    }
}

/// Die Gate-Zeile für `--require-in-scope`, falls verfehlt — sonst `None`.
///
/// Fail-closed: Hat eine Session keinen lesbaren Bereich, oder ist der
/// Commit nicht abgeglichen (seine Pfade fehlen dann), ist das Gate nicht
/// bestanden. Sonst zählt jeder Pfad außerhalb eines Bereichs, über alle
/// Sessions des Laufs einmal.
pub(super) fn gate_failure(states: &[ScopeState], artifact: &ArtifactState) -> Option<String> {
    let not_assessed =
        |why: &str| format!("Gate           scope not assessed ({why}) — required in scope");
    if let Some(why) = states.iter().find_map(|state| match state {
        ScopeState::NotAssessed(why) => Some(*why),
        ScopeState::Assessed(_) => None,
    }) {
        return Some(not_assessed(why.word()));
    }
    match artifact {
        ArtifactState::Assessed(_) => {}
        ArtifactState::NoCommit => return Some(not_assessed("no linked commit")),
        ArtifactState::Unavailable(why) => return Some(not_assessed(why)),
        ArtifactState::Failed(_) => return Some(not_assessed("error")),
    }
    let outside = outside_paths(states);
    (outside > 0).then(|| {
        format!(
            "Gate           {outside} path(s) {} — required in scope",
            Finding::OutOfScope.word()
        )
    })
}

/// Wie viele verschiedene Pfade außerhalb eines Bereichs liegen, über alle
/// Sessions des Laufs — ein Pfad, den mehrere Sessions nennen, zählt einmal.
fn outside_paths(states: &[ScopeState]) -> usize {
    states
        .iter()
        .flat_map(|state| match state {
            ScopeState::Assessed(findings) => findings.as_slice(),
            ScopeState::NotAssessed(_) => &[],
        })
        .map(|finding| finding.path.as_str())
        .collect::<BTreeSet<&str>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_reader::scope::ScopeSource;

    fn finding(path: &str, sources: &[ScopeSource]) -> ScopeFinding {
        ScopeFinding {
            path: path.into(),
            sources: sources.to_vec(),
        }
    }

    /// Eine Session mit Schreib-Claims auf `paths`, aufgenommen in `/repo`.
    fn writing(paths: &[&str]) -> Session {
        let mut session = Session::new(
            minds_core::Agent {
                name: "claude-code".into(),
                version: "test".into(),
            },
            minds_core::Model {
                provider: "test".into(),
                id: "test".into(),
            },
            minds_core::Intent::default(),
        );
        session.lineage = Some(minds_core::Lineage {
            local_id: "local".into(),
            started_at: None,
            ended_at: None,
            cwd: Some("/repo".into()),
            closed: false,
        });
        session.turns.push(minds_core::Turn {
            role: minds_core::Role::Assistant,
            text: String::new(),
            tool_calls: paths
                .iter()
                .map(|path| minds_core::ToolCall {
                    name: "Write".into(),
                    arguments: "{}".into(),
                    capture: None,
                    effect: Some(minds_core::Effect {
                        kind: EffectKind::Write,
                        path: Some((*path).into()),
                        content: None,
                        written: None,
                        written_unavailable: None,
                    }),
                })
                .collect(),
            parent: None,
            at: None,
        });
        session
    }

    #[test]
    fn unresolvable_claims_below_the_root_are_named() {
        let roots = [Path::new("/repo")];
        let known = |_: &str| false;
        let check = |paths: &[&str]| unresolved_claim(&writing(paths), &roots, &known);
        assert!(!check(&["/repo/src/a.rs", "src/b.rs"]));
        // Außerhalb des Checkouts: keine Scope-Frage.
        assert!(!check(&["/elsewhere/plan.md", "~/.claude/plans/x.md"]));
        // Unterhalb der Wurzel, aber keinem Pfad zuzuordnen.
        assert!(check(&["/repo/src/../.github/workflows/ci.yml"]));
        assert!(check(&["src/../../x"]));
    }

    #[test]
    fn detail_lines_align_and_name_their_sources() {
        let state = ScopeState::Assessed(vec![
            finding(
                "docs/README.md",
                &[ScopeSource::Commit, ScopeSource::Observation],
            ),
            finding("a\u{1b}[31m.rs", &[ScopeSource::Claim]),
        ]);
        assert_eq!(
            detail_lines(&state, false, "unused"),
            [
                "  out of scope   docs/README.md  (commit, observation)",
                "  out of scope   a\\u{1b}[31m.rs  (claim)",
            ]
        );
        assert_eq!(segment(&state).as_deref(), Some("2 out of scope"));
        assert_eq!(
            segment(&ScopeState::Assessed(Vec::new())).as_deref(),
            Some("0 out of scope")
        );
        assert_eq!(
            segment(&ScopeState::NotAssessed(NoScope::NotDeclared)),
            None
        );
        assert_eq!(
            segment(&ScopeState::NotAssessed(NoScope::IntentNotBound)),
            None
        );
        assert_eq!(
            segment(&ScopeState::NotAssessed(NoScope::AnchorUnreadable)).as_deref(),
            Some("scope not assessed (intent anchor unreadable)")
        );
        assert_eq!(
            segment(&ScopeState::NotAssessed(NoScope::AnchorMissing)).as_deref(),
            Some("scope not assessed (intent anchor not in this store)")
        );
        assert_eq!(
            segment(&ScopeState::NotAssessed(NoScope::ObservationsIncomplete)).as_deref(),
            Some("scope not assessed (witness observations incomplete)")
        );
    }

    #[test]
    fn detail_lines_are_capped() {
        let findings: Vec<ScopeFinding> = (0..DETAIL_CAP + 3)
            .map(|i| finding(&format!("f{i:02}"), &[ScopeSource::Commit]))
            .collect();
        let state = ScopeState::Assessed(findings);
        let capped = detail_lines(&state, false, "minds verify x --all");
        assert_eq!(capped.len(), DETAIL_CAP + 1);
        assert_eq!(capped[DETAIL_CAP], "  … 3 more (minds verify x --all)");
        assert_eq!(detail_lines(&state, true, "-").len(), DETAIL_CAP + 3);
    }

    #[test]
    fn the_gate_counts_each_path_once() {
        let state = |paths: &[&str]| {
            ScopeState::Assessed(
                paths
                    .iter()
                    .map(|p| finding(p, &[ScopeSource::Claim]))
                    .collect(),
            )
        };
        let states = [
            state(&["docs/README.md", "LICENSE"]),
            state(&["docs/README.md"]),
            ScopeState::Assessed(Vec::new()),
        ];
        assert_eq!(outside_paths(&states), 2);
        assert_eq!(outside_paths(&[state(&[])]), 0);
    }

    #[test]
    fn the_gate_is_fail_closed() {
        let commit = ArtifactState::NoCommit;
        assert_eq!(
            gate_failure(&[ScopeState::NotAssessed(NoScope::NotDeclared)], &commit).as_deref(),
            Some("Gate           scope not assessed (no scope declared) — required in scope")
        );
        assert_eq!(
            gate_failure(&[ScopeState::Assessed(Vec::new())], &commit).as_deref(),
            Some("Gate           scope not assessed (no linked commit) — required in scope")
        );
        assert_eq!(
            gate_failure(
                &[ScopeState::Assessed(Vec::new())],
                &ArtifactState::Unavailable("first parent not in this clone (shallow)")
            )
            .as_deref(),
            Some(
                "Gate           scope not assessed (first parent not in this clone (shallow)) — required in scope"
            )
        );
        // Die Zählung über einen abgeglichenen Commit prüft der
        // End-to-End-Test.
    }
}
