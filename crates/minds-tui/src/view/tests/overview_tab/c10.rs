//! C10 · Die Why-Kette ist abgeschafft: ihre Versprechen über Commits leben
//! im EVIDENCE-Block des Overview-Details weiter — jede Probe nennt den
//! Test, aus dem sie umgezogen ist.

use super::*;
use minds_reader::changes::{LineClasses, ReasonCount};

/// Eine Quelle, deren Urteil fest ist.
struct Judged(crate::VerifyVerdict);

impl crate::Source for Judged {
    fn load(&self) -> minds_reader::Result<Inspection> {
        unreachable!("this test never reloads")
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        None
    }
    fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
        Ok(crate::CommitVerify {
            verdict: Ok(self.0),
            sessions: Vec::new(),
            out_of_scope: None,
            out_of_scope_paths: Vec::new(),
            scope_note: None,
            conformance: None,
        })
    }
}

fn text_at(app: &mut App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Wählt den Agent-Commit und setzt seine Zeilenklassen, wie
/// `fill_overview` sie gerechnet hätte.
fn on_agent_commit(app: &mut App, commit: CommitId, classes: LineClasses) {
    app.open_overview();
    let state = app.overview.as_mut().unwrap();
    state.select_commit(commit);
    state.lines.insert(commit, Ok(classes));
}

fn classes(explained: usize, unexplained: usize, reasons: Vec<ReasonCount>) -> LineClasses {
    LineClasses {
        added: explained + unexplained,
        explained,
        fs_only: 0,
        stated: 0,
        unexplained,
        not_assessed: 0,
        skipped_files: 0,
        files: 1,
        reasons,
    }
}

/// Eine lückenlose Kette **sagt** das — umgezogen aus
/// `a_fully_backed_chain_says_so_and_focus_explains_the_evidence_without_enter`
/// (`✓ no gap`): Geprüft und ganz beobachtet steht in EVIDENCE ✓ an
/// INTEGRITY und COVERAGE, und die Fußzeile bietet die Entstehung an.
#[test]
fn c10_a_fully_backed_commit_says_so_in_evidence() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0, Vec::new()));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    let line = |label: &str| {
        out.lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("{label}: {out}"))
            .to_string()
    };
    assert!(line("INTEGRITY").contains("✓ VERIFIED"), "{out}");
    assert!(
        line("COVERAGE").contains("✓ all 2 added lines observed"),
        "{out}"
    );
    // Keine Lücke heißt: kein ⚠ und kein ◦ an diesen Achsen.
    assert!(!line("INTEGRITY").contains('⚠'), "{out}");
    assert!(!line("COVERAGE").contains('◦'), "{out}");
    assert!(!out.contains(" NO GAP "), "{out}");
    assert!(out.contains("Enter how it was made"), "{out}");
}

/// Eine Lücke wird benannt, nicht versteckt — umgezogen aus
/// `why_shows_the_chain_and_a_missing_link_is_named_not_hidden`: Jede
/// `◦`-Zeile in EVIDENCE nennt ihren Grund.
#[test]
fn c10_a_gap_in_evidence_is_named_not_hidden() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(
        &mut app,
        commits[1],
        classes(
            1,
            1,
            vec![ReasonCount {
                reason: "no tool claim or shell mention found",
                lines: 1,
            }],
        ),
    );
    let out = text_at(&mut app, 160, 40);
    assert!(out.contains("◦ 1 of 2 added lines unexplained"), "{out}");
    assert!(
        out.contains("— no tool claim or shell mention found: all 1"),
        "{out}"
    );
    // Die Aktion steht in NEXT (C3.7) — nicht mehr als Unterzeile der Achse.
    assert!(out.contains("NEXT"), "{out}");
    assert!(!out.contains("→ d, then n"), "{out}");
}

/// Ein fehlender Review ist **nie** ein Gap: `◦ not reviewed yet`, kein `⚠`
/// — die Kette rahmte ihn als Lücke (`⚠ REVIEW`, „No review — nobody has
/// decided"), das war falsch (C10).
#[test]
fn c10_a_missing_review_is_never_a_gap() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0, Vec::new()));
    let out = text_at(&mut app, 160, 40);
    let review = out
        .lines()
        .rfind(|l| l.contains("REVIEW"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(review.contains("◦ not reviewed yet"), "{out}");
    assert!(!review.contains('⚠'), "{out}");
    assert!(!out.contains("⚠ REVIEW"), "{out}");
    assert!(!out.contains("No review — nobody has decided"), "{out}");
    assert!(!out.contains("GAP"), "{out}");
}
