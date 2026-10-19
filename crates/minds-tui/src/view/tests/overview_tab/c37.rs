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
fn c37_an_unproven_binding_points_to_the_requirements_tab() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    // C12.2 (Klasse 2): der Tab heißt Requirements, nicht mehr Intent.
    assert!(
        out.contains("→ i: Requirements tab — the binding is not proven"),
        "{out}"
    );
    assert!(!out.contains("Intent tab"), "{out}");
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
    // Das Subjekt ist die Session (kein Change-Id-Trailer in der Fixture).
    let out = text_at(&mut app, 300, 40);
    assert!(
        out.contains(&format!("→ minds review {} …", sid('a'))),
        "{out}"
    );
    assert!(out.contains("nobody has decided yet"), "{out}");
    let (next, _) = next_of(&out);
    assert_eq!(next.len(), 2, "{out}");
}

/// Das Subjekt von `minds review` ist eine Session- oder Change-Id — ein
/// Kommando, das es gibt, nie der Commit (den nimmt `minds review` nicht).
#[test]
fn c37_the_review_command_names_a_subject_minds_review_accepts() {
    use minds_core::{ChangeId, SessionId};
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(&inspection, Vec::new());
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 300, 40);
    let line = out
        .lines()
        .find(|l| l.contains("NEXT") && l.contains("→ minds review "))
        .unwrap_or_else(|| panic!("{out}"));
    let after = &line[line.find("→ minds review ").unwrap() + "→ minds review ".len()..];
    let subject = after.split(" …").next().unwrap_or("").trim();
    assert!(
        subject.parse::<SessionId>().is_ok() || subject.parse::<ChangeId>().is_ok(),
        "{subject:?} is neither a session nor a change id\n{out}"
    );
    let short = &commits[1].to_string()[..7];
    assert!(!line.contains(&format!("review {short}")), "{line}");
}

/// Ein abgelehnter Review ist kein „nichts offen“.
#[test]
fn c37_a_rejected_review_is_named() {
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(
        &inspection,
        vec![Review::new(
            Subject::Session(sid('a').to_string()),
            Decision::Reject,
            "pd",
            "falscher Ansatz",
            Some("2026-10-17T10:00:00Z".into()),
        )],
    );
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ Enter: how it was made — the review rejected it"),
        "{out}"
    );
}

/// Ungedeutete Aufrufe: `e:` mit der Zahl — vor REQUIREMENT und REVIEW.
#[test]
fn c37_uninterpreted_calls_point_to_the_evidence() {
    let (_dir, repo, commits, inspection) = history();
    // Session a bekommt einen beobachteten, nicht gedeuteten Aufruf.
    let index = inspection.index().clone();
    let mut sessions = BTreeMap::new();
    let mut links = BTreeMap::new();
    for (id, session) in index.sessions() {
        let mut session = session.clone();
        session.intent_events.clear();
        if *id == sid('a') {
            session.turns[0].tool_calls.push(ToolCall {
                outcome: None,
                capture: Some(minds_core::Capture {
                    status: minds_core::CaptureStatus::Uninterpreted,
                    adapter: "generic".into(),
                    adapter_version: 1,
                    note: None,
                }),
                name: "apply_patch".into(),
                arguments: "{}".into(),
                effect: None,
            });
        }
        sessions.insert(*id, session);
        for commit in index.commits_of(*id) {
            links.entry(commit).or_insert_with(Vec::new).push(*id);
        }
    }
    let inspection = Inspection::from_index(Index::from_parts(sessions, links), Vec::new(), "demo");
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ e: evidence — 1 uninterpreted call"),
        "{out}"
    );
    assert!(!out.contains("→ minds review"), "{out}");
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

/// Solange die Übersicht die Zeilen noch zählt, ist COVERAGE nicht ✓ — NEXT
/// sagt das und empfiehlt nichts weiter unten.
#[test]
fn c37_while_counting_next_waits() {
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(&inspection, Vec::new());
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    app.overview.as_mut().unwrap().select_commit(commits[1]);
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ wait — the lines are still being counted"),
        "{out}"
    );
    assert!(!out.contains("→ minds review"), "{out}");
    assert!(!out.contains("nothing is open"), "{out}");
}

/// Der Anker ist noch nicht gelesen (`… checking`): NEXT wartet — nichts
/// darunter wird empfohlen.
#[test]
fn c37_while_the_requirement_loads_next_waits() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    // Ein Urteil mit belegter Bindung — nur der Anker fehlt noch.
    struct Bound;
    impl crate::Source for Bound {
        fn load(&self) -> minds_reader::Result<Inspection> {
            unreachable!()
        }
        fn stamp(&self) -> Option<crate::Stamp> {
            None
        }
        fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
            Ok(crate::CommitVerify {
                verdict: Ok(crate::VerifyVerdict::Verified),
                sessions: vec![crate::SessionAssurance {
                    session: sid('a'),
                    level: minds_reader::assurance::Assurance::A0Claimed,
                    reason: None,
                    intent: minds_reader::assurance::IntentState::Bound {
                        anchor_id: minds_core::ContentHash::from_bytes([1; 32]),
                        chained: true,
                        signature: minds_reader::assurance::IntentSignature::Unsigned,
                        snapshot_matches: true,
                        from_session_start: true,
                        changed_mid_session: false,
                    },
                    tampered: false,
                }],
                out_of_scope: None,
                out_of_scope_paths: Vec::new(),
                scope_note: None,
                conformance: None,
            })
        }
    }
    assert!(app.fill_overview_verify(&Bound));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ wait — the requirement is still being read"),
        "{out}"
    );
    assert!(!out.contains("nothing is open"), "{out}");
    assert!(!out.contains("→ minds review"), "{out}");
}

/// Berichtete Zeilen ohne Hash: `d:` mit dem Spec-Wortlaut.
#[test]
fn c37_reported_lines_point_to_the_diff_too() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = App::new(inspection, &repo, None);
    let mut stated = classes(0, 0);
    stated.added = 95;
    stated.stated = 95;
    on_agent_commit(&mut app, commits[1], stated);
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ d: inspect the 95 lines the agent claims without a hash"),
        "{out}"
    );
}

/// Ein Review mit Befund ist kein „nichts offen“: Enter, mit dem Befund.
#[test]
fn c37_a_review_with_findings_is_named_with_enter() {
    let (_dir, repo, commits, inspection) = history();
    let inspection = without_anchor(
        &inspection,
        vec![Review::new(
            Subject::Session(sid('a').to_string()),
            Decision::NeedsWork,
            "pd",
            "Retry bei 5xx fehlt",
            Some("2026-10-17T10:00:00Z".into()),
        )],
    );
    let mut app = App::new(inspection, &repo, None);
    on_agent_commit(&mut app, commits[1], classes(2, 0));
    assert!(app.fill_overview_verify(&Judged(crate::VerifyVerdict::Verified)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("→ Enter: how it was made — the review says needs work"),
        "{out}"
    );
    assert!(!out.contains("nothing is open"), "{out}");
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
