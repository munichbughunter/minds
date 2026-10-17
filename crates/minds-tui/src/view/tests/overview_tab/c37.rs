//! C3.7 · Die nächste Aktion steht im Detail: Die oberste Zeile in EVIDENCE,
//! die nicht ✓ ist, bestimmt sie — eine NEXT-Zeile, nie mehr; dieselbe steht
//! in Fußzeile 1.

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

fn classes(explained: usize, unexplained: usize) -> LineClasses {
    LineClasses {
        added: explained + unexplained,
        explained,
        fs_only: 0,
        stated: 0,
        unexplained,
        not_assessed: 0,
        skipped_files: 0,
        files: 1,
        reasons: if unexplained > 0 {
            vec![ReasonCount {
                reason: "no tool claim or shell mention found",
                lines: unexplained,
            }]
        } else {
            Vec::new()
        },
    }
}

fn on_agent_commit(app: &mut App, commit: CommitId, classes: LineClasses) {
    app.open_overview();
    let state = app.overview.as_mut().unwrap();
    state.select_commit(commit);
    state.lines.insert(commit, Ok(classes));
}

/// Die NEXT-Zeilen des Details und Fußzeile 1.
fn next_of(out: &str) -> (Vec<String>, String) {
    let next: Vec<String> = out
        .lines()
        .filter(|l| l.contains("NEXT"))
        .map(|l| l.to_string())
        .collect();
    let footer = out.lines().rev().nth(1).unwrap_or("").to_string();
    (next, footer)
}

/// Dieselbe Historie, aber ohne Anker an Session a — und mit `reviews`.
fn without_anchor(inspection: &Inspection, reviews: Vec<Review>) -> Inspection {
    let index = inspection.index().clone();
    let mut sessions = BTreeMap::new();
    let mut commits = BTreeMap::new();
    for (id, session) in index.sessions() {
        let mut session = session.clone();
        session.intent_events.clear();
        session.intent_anchor = None;
        sessions.insert(*id, session);
        for commit in index.commits_of(*id) {
            commits.entry(commit).or_insert_with(Vec::new).push(*id);
        }
    }
    Inspection::from_index(Index::from_parts(sessions, commits), reviews, "demo")
}

/// Vor dem Urteil: `v: run verify` — eine NEXT-Zeile im Detail (die Zeile im
/// Detail und die in der Fußzeile sagen dasselbe).
#[test]
fn c37_before_the_verdict_next_is_verify() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    let out = text_at(&mut app, 160, 40);
    let (next, footer) = next_of(&out);
    // Detail und Fußzeile: zwei Zeilen, ein Text.
    assert_eq!(next.len(), 2, "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("NEXT") && l.contains("→ v: run verify") && !l.contains("updated")),
        "{out}"
    );
    assert!(footer.contains("NEXT → v: run verify"), "{out}");
}

/// Geprüft, aber eine Zeile unerklärt: `d:` mit der Zahl — und `n` als
/// Hinweis, dass es weitergeht.
#[test]
fn c37_unexplained_lines_point_to_the_diff() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(1, 1));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    let (next, _) = next_of(&out);
    assert_eq!(next.len(), 2, "{out}");
    assert!(
        out.contains("→ d: inspect the 1 line not observed (n jumps on)"),
        "{out}"
    );
    assert!(!out.contains("→ v: run verify"), "{out}");
}

/// Geprüft, alles beobachtet, aber der Anker ist nicht belegt gebunden:
/// `i:` — REQUIREMENT kommt vor REVIEW.
#[test]
fn c37_an_unproven_binding_points_to_the_intent_tab() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ i: Intent tab — the binding is not proven"),
        "{out}"
    );
    assert!(!out.contains("→ minds review"), "{out}");
}

/// Ohne Anker, geprüft, alles beobachtet: der offene Review ist die letzte
/// offene Entscheidung — als Text, die TUI schreibt nicht.
#[test]
fn c37_an_open_review_is_the_last_open_decision() {
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(&inspection, Vec::new());
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    let short = &commits[1].to_string()[..7];
    assert!(
        out.contains(&format!(
            "→ minds review {short} … — nobody has decided yet"
        )),
        "{out}"
    );
    let (next, _) = next_of(&out);
    assert_eq!(next.len(), 2, "{out}");
}

/// Alles ✓: nur noch nachvollziehen — `Enter`.
#[test]
fn c37_all_checked_leaves_only_enter() {
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(
        &inspection,
        vec![Review::new(
            Subject::Session(sid('a').to_string()),
            Decision::Approve,
            "pd",
            "sieht gut aus",
            Some("2026-10-17T10:00:00Z".into()),
        )],
    );
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ Enter: how it was made — nothing is open"),
        "{out}"
    );
    let (next, footer) = next_of(&out);
    assert_eq!(next.len(), 2, "{out}");
    assert!(footer.contains("NEXT → Enter: how it was made"), "{out}");
}

/// Manipuliert: `e:` — vor allem anderen, auch vor unerklärten Zeilen.
#[test]
fn c37_tampered_points_to_the_evidence_first() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(1, 1));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Tampered)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ e: evidence of the session — what was altered"),
        "{out}"
    );
    assert!(!out.contains("→ d: inspect"), "{out}");
}
