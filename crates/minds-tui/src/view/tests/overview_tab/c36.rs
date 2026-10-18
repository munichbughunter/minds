//! C3.6 · Prompt als eigener Abschnitt, Gründe für „unexplained", kein
//! „unknown": Das Wichtigste steht nicht als Fußnote; jede offene Zeile
//! nennt ihren Grund; ein Platzhalter aus `minds-capture` wird nie gedruckt.

use super::*;
use crate::theme;

fn lines_at(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

/// Das Detail rechts der Liste — bzw. die ganze Fläche, wenn es keine
/// Liste daneben gibt (schmal).
fn detail_text(app: &mut App, w: u16, h: u16) -> String {
    let lines = lines_at(app, w, h);
    let edge = lines[2]
        .find("┐┌")
        .map(|b| lines[2][..b].chars().count() + 1);
    lines
        .iter()
        .map(|l| match edge {
            Some(e) => l.chars().skip(e).collect::<String>(),
            None => l.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Der SESSIONS-Block: zwischen seiner Überschrift und EVIDENCE.
fn sessions_block(detail: &str) -> String {
    let start = detail
        .find("SESSIONS (")
        .unwrap_or_else(|| panic!("{detail}"));
    let rest = &detail[start..];
    let end = rest.find("EVIDENCE").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Eine Quelle ohne Anker und ohne Urteil — das Detail rechnet nur, was
/// der Reader hergibt.
struct Plain;

impl crate::Source for Plain {
    fn load(&self) -> minds_reader::Result<Inspection> {
        unreachable!("this test never reloads")
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        None
    }
    fn intents(&self, _named: &[minds_core::ContentHash]) -> Result<crate::IntentList, String> {
        Ok(crate::IntentList {
            total: 0,
            entries: Vec::new(),
            skipped: 0,
        })
    }
    fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
        Err("minds verify not found".into())
    }
}

/// Ein echter Commit mit zwei Dateien (vier hinzugefügte Zeilen), auf zwei
/// Branches und mit Change-Id — der Kopf des Details ist damit so lang wie
/// im Alltag (Branches umgebrochen, Change-Zeile), nicht so kurz wie in
/// einer Minimal-Fixture. Die Sessions tragen keinen Dateiclaim — wie der
/// Screenshot vom 15.10.
fn linked(sessions_in: Vec<(SessionId, Session)>) -> (tempfile::TempDir, Repo, Inspection) {
    let (dir, _) = repo();
    std::fs::write(dir.path().join("a"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.path().join("b"), "four\n").unwrap();
    git(dir.path(), &["add", "a", "b"]);
    git(dir.path(), &["commit", "-q", "-m", "feat: two files"]);
    git(dir.path(), &["branch", "feat/two-files-with-a-long-name"]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let mut sessions = BTreeMap::new();
    let mut ids = Vec::new();
    for (id, mut s) in sessions_in {
        s.produced.files.clear();
        s.turns[0].tool_calls.clear();
        ids.push(id);
        sessions.insert(id, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(head, ids.clone());
    let change: minds_core::ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
    let mut changes = BTreeMap::new();
    changes.insert(head, change);
    let mut index = Index::from_parts(sessions, commits).with_changes(changes);
    for id in ids {
        let (seal_id, seal) = clean_seal(id);
        index = index.with_seals(id, vec![(seal_id, seal, false)]);
    }
    let inspection = Inspection::from_index(index, vec![], "r");
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo, inspection)
}

/// Eine Session ohne Dateiclaim mit diesem Auftrag und Modell.
fn without_claims(request: &str, model: (&str, &str)) -> (tempfile::TempDir, Repo, Inspection) {
    let mut s = session(request, "2026-10-02T10:00:00Z");
    s.model = minds_core::Model {
        provider: model.0.into(),
        id: model.1.into(),
    };
    linked(vec![(sid('a'), s)])
}

/// Die Übersicht auf HEAD, das Detail vollständig gelesen.
fn on_head<'r>(repo: &'r Repo, inspection: Inspection) -> App<'r> {
    let mut app = App::new(inspection, repo, None);
    app.open_overview();
    while app.fill_overview(&Plain) {}
    app
}

fn twenty_lines() -> String {
    (1..=20)
        .map(|i| format!("line {i:02} of the request"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// AC1: PROMPT ist ein eigener Abschnitt vor SESSIONS, als Claim
/// gekennzeichnet, mit Volltext bis 12 Zeilen und `… N more lines`; Space
/// klappt auf. Der Prompt steht nicht zusätzlich im SESSIONS-Block.
#[test]
fn c36_the_prompt_is_its_own_section_not_a_footnote() {
    let (_dir, repo, inspection) = without_claims(&twenty_lines(), ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let detail = detail_text(&mut app, w, h);
        assert!(detail.contains("PROMPT"), "{w}: {detail}");
        assert!(
            detail.contains("◌ as recorded by the agent · session b3-aaaaaaaa"),
            "{w}: {detail}"
        );
        assert!(detail.contains("line 01 of the request"), "{w}: {detail}");
        assert!(
            detail.find("PROMPT").unwrap() < detail.find("SESSIONS (").unwrap(),
            "{w}: {detail}"
        );
        assert!(
            !sessions_block(&detail).contains("of the request"),
            "{w}: {detail}"
        );
    }
    // Mit Platz die vollen 12 Zeilen; bei 160×40 so viele, wie EVIDENCE
    // darunter lässt — nie mehr als 12.
    let detail = detail_text(&mut app, 160, 60);
    assert!(detail.contains("line 12 of the request"), "{detail}");
    assert!(!detail.contains("line 13 of the request"), "{detail}");
    assert!(detail.contains("… 8 more lines"), "{detail}");
    let detail = detail_text(&mut app, 160, 40);
    let shown = (1..=20)
        .filter(|i| detail.contains(&format!("line {i:02} of the request")))
        .count();
    assert!((1..=12).contains(&shown), "{shown}: {detail}");
    assert!(
        detail.contains(&format!("… {} more lines", 20 - shown)),
        "{detail}"
    );
    assert!(detail.contains("REVIEW"), "{detail}");
    app.reduce(Action::Space);
    let open = detail_text(&mut app, 160, 60);
    assert!(open.contains("line 20 of the request"), "{open}");
    assert!(!open.contains("more lines"), "{open}");
    app.reduce(Action::Space);
    assert!(detail_text(&mut app, 160, 60).contains("… 8 more lines"));
}

/// AC1 bei 80 Spalten: Das Detail ist eine Ebene hinter Enter — und zeigt
/// den PROMPT ebenso. In 24 Zeilen hat neben dem langen Kopf und EVIDENCE
/// kein Prompt-Text Platz: Der Kopf sagt, wie viel fehlt; Space zeigt ihn.
#[test]
fn c36_the_prompt_is_there_in_a_narrow_terminal_too() {
    let (_dir, repo, inspection) = without_claims(&twenty_lines(), ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    let _ = lines_at(&mut app, 80, 24);
    app.reduce(Action::Enter);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("PROMPT"), "{out}");
    assert!(out.contains("… 20 lines · Space shows all"), "{out}");
    assert!(out.contains("INTEGRITY"), "{out}");
    assert!(out.contains("? help"), "{out}");
    app.reduce(Action::Space);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("line 01 of the request"), "{out}");
    assert!(!out.contains("Space shows all"), "{out}");
}

/// AC2 + AC3: COVERAGE nennt den häufigsten Grund mit Zahl und die
/// fehlenden Dateiclaims; die Gründe summieren sich zur Zahl des
/// Changes-Tabs. SESSIONS benennt den Widerspruch — gedimmt, nie rot.
#[test]
fn c36_coverage_names_its_reason_and_the_missing_claims() {
    let (_dir, repo, inspection) = without_claims("Write the files", ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    let commit = app.overview.as_ref().unwrap().agent_commit().unwrap().id;
    let detail = detail_text(&mut app, 160, 40);
    assert!(
        detail.contains("◦ 4 of 4 added lines unexplained"),
        "{detail}"
    );
    assert!(
        detail.contains("no tool claim or shell mention found"),
        "{detail}"
    );
    assert!(detail.contains("all 4"), "{detail}");
    assert!(detail.contains("session records 0 files"), "{detail}");
    // Die Aktion steht in NEXT (C3.7), nicht als Unterzeile der Achse.
    assert!(!detail.contains("→ d, then n"), "{detail}");
    assert!(detail.contains("NEXT"), "{detail}");
    // Eine Quelle: die Gründe summieren sich zu `◦n` des Changes-Tabs.
    let set = app.inspection.changes(&repo, commit).unwrap();
    let classes = set.line_classes();
    let sum: usize = classes.reasons.iter().map(|r| r.lines).sum();
    assert_eq!(sum, set.unexplained());
    assert_eq!(sum, 4);
    // Der Widerspruch steht in SESSIONS, als Lücke, nicht als Befund — auch
    // bei 120 Spalten ganz, nie zerschnitten.
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let detail = detail_text(&mut app, w, h);
        let sessions = sessions_block(&detail);
        assert!(
            sessions.contains("0 files claimed · commit changes 2 files"),
            "{w}: {detail}"
        );
        assert!(
            detail.contains("no tool claim or shell mention found"),
            "{w}: {detail}"
        );
    }
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut seen = (false, false);
    for y in 0..40u16 {
        let row: String = (0..160u16).map(|x| buffer[(x, y)].symbol()).collect();
        if let Some(at) = row.find("files claimed") {
            seen.0 = true;
            let x0 = u16::try_from(row[..at].chars().count()).unwrap();
            for x in x0..x0 + 13 {
                assert_ne!(buffer[(x, y)].fg, theme::DELETE, "a gap is not red: {row}");
            }
        }
        // Der Claim-Kopf des PROMPT ist nie OK-grün.
        if let Some(at) = row.find("as recorded by the agent") {
            seen.1 = true;
            let x0 = u16::try_from(row[..at].chars().count()).unwrap();
            for x in x0..x0 + 24 {
                assert_ne!(buffer[(x, y)].fg, theme::OK, "a claim is not green: {row}");
            }
        }
    }
    assert_eq!(seen, (true, true));
}

/// Review: Der zugeklappte Prompt drängt die Trust-Achsen nie aus dem
/// Bild — bei 120×30 und 160×40 stehen alle fünf ohne Scrollen da; bei
/// 80×24 (Detail hinter Enter) blättert PgDn bis REVIEW.
#[test]
fn c36_the_collapsed_prompt_never_hides_the_evidence_axes() {
    const AXES: [&str; 5] = [
        "INTEGRITY",
        "COVERAGE",
        "INTERPRETATION",
        "REQUIREMENT",
        "REVIEW",
    ];
    let (_dir, repo, inspection) = without_claims(&twenty_lines(), ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let detail = detail_text(&mut app, w, h);
        assert!(detail.contains("PROMPT"), "{w}: {detail}");
        assert!(detail.contains("more lines"), "{w}: {detail}");
        for axis in AXES {
            assert!(detail.contains(axis), "{w}: {axis} missing: {detail}");
        }
    }
    // Die Fußzeile bietet `Space prompt`, weil der letzte Draw kürzte; der
    // Hinweis steht auch in der Kürzungszeile selbst.
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains("Space prompt"), "{out}");
    assert!(out.contains("more lines · Space shows all"), "{out}");
    let _ = lines_at(&mut app, 80, 24);
    app.reduce(Action::Enter);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("PROMPT"), "{out}");
    assert!(out.contains("INTEGRITY"), "{out}");
    app.reduce(Action::PageDown);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("REVIEW"), "{out}");
}

/// Review: Mehrere Sessions mit langen Prompts bei 120×30 — der zweite
/// Pfad der Fensterrechnung: eine Karte mit PROMPT-Unterabschnitt, die
/// übrigen gezählt, alle fünf Achsen sichtbar.
#[test]
fn c36_several_long_prompts_keep_the_axes_at_120_columns() {
    const AXES: [&str; 5] = [
        "INTEGRITY",
        "COVERAGE",
        "INTERPRETATION",
        "REQUIREMENT",
        "REVIEW",
    ];
    let (_dir, repo, inspection) = linked(vec![
        (sid('a'), session(&twenty_lines(), "2026-10-02T10:00:00Z")),
        (sid('b'), session(&twenty_lines(), "2026-10-02T11:00:00Z")),
    ]);
    let mut app = on_head(&repo, inspection);
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let detail = detail_text(&mut app, w, h);
        assert!(detail.contains("SESSIONS (2)"), "{w}: {detail}");
        assert!(detail.contains("PROMPT"), "{w}: {detail}");
        assert!(detail.contains("more lines"), "{w}: {detail}");
        for axis in AXES {
            assert!(detail.contains(axis), "{w}: {axis} missing: {detail}");
        }
    }
}

/// Review: Punktgrößen fangen Modulo-Fälle der Fensterrechnung nicht —
/// ein Höhen-Sweep über 1, 2 und 3 Sessions mit langen Prompts, 120 und
/// 160 Spalten, Cursor nacheinander auf jeder Session: Die fünf Achsen
/// stehen immer im Bild, sobald das Detail sie überhaupt fassen kann.
#[test]
fn c36_the_axes_survive_every_height_and_session_cursor() {
    const AXES: [&str; 5] = [
        "INTEGRITY",
        "COVERAGE",
        "INTERPRETATION",
        "REQUIREMENT",
        "REVIEW",
    ];
    for n in 1..=3usize {
        let sessions = (0..n)
            .map(|i| {
                let id = sid(char::from(b'a' + u8::try_from(i).unwrap()));
                (id, session(&twenty_lines(), "2026-10-02T10:00:00Z"))
            })
            .collect();
        let (_dir, repo, inspection) = linked(sessions);
        for cursor in 0..n {
            let mut app = on_head(&repo, inspection.clone());
            if n > 1 {
                // Enter wählt bei mehreren Sessions zuerst (Fokus rechts).
                app.reduce(Action::Enter);
                for _ in 0..cursor {
                    app.reduce(Action::Down);
                }
            }
            for w in [120u16, 160] {
                for h in 30..=60u16 {
                    let detail = detail_text(&mut app, w, h);
                    assert!(detail.contains("PROMPT"), "{n}/{cursor} {w}x{h}: {detail}");
                    for axis in AXES {
                        assert!(
                            detail.contains(axis),
                            "{n} sessions, cursor {cursor}, {w}x{h}: {axis} missing: {detail}"
                        );
                    }
                }
            }
        }
    }
}

/// Review: Ein Cursorwechsel vergisst, dass der vorige Commit kürzte —
/// Space auf einem Commit mit kurzem Prompt bleibt ohne Draw ein No-op,
/// und die schmale Listenebene bietet kein veraltetes `Space prompt`.
#[test]
fn c36_a_stale_clipped_flag_never_survives_a_cursor_move_or_the_narrow_list() {
    // Zwei Agent-Commits: unten einer mit langem Prompt, auf HEAD einer
    // mit kurzem.
    let (dir, _) = repo();
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "feat: long"],
    );
    let long: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "feat: short"],
    );
    let short: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), session(&twenty_lines(), "2026-10-02T10:00:00Z"));
    sessions.insert(sid('b'), session("short", "2026-10-02T11:00:00Z"));
    let mut commits = BTreeMap::new();
    commits.insert(long, vec![sid('a')]);
    commits.insert(short, vec![sid('b')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "r");
    let repo = Repo::open(dir.path()).unwrap();
    let mut app = on_head(&repo, inspection);
    assert!(app.overview.as_mut().unwrap().select_commit(long));
    while app.fill_overview(&Plain) {}
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains("Space prompt"), "{out}");
    // Schmal, Listenebene: kein Hinweis aus dem breiten Draw.
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(!out.contains("Space prompt"), "{out}");
    assert!(!app.overview.as_ref().unwrap().prompt_clipped.get());
    // Breit gezeichnet (kürzt), dann der Cursor auf HEAD und Space ohne
    // neuen Draw: der kurze Prompt klappt nichts auf.
    let _ = lines_at(&mut app, 160, 40);
    assert!(app.overview.as_ref().unwrap().prompt_clipped.get());
    app.reduce(Action::Up);
    assert_eq!(
        app.overview.as_ref().unwrap().agent_commit().map(|r| r.id),
        Some(short)
    );
    app.reduce(Action::Space);
    let state = app.overview.as_ref().unwrap();
    assert!(!state.prompt_open, "a cursor move forgets the old clipping");
    // Nach dem Draw auf HEAD: nichts gekürzt, kein Hinweis.
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(!out.contains("Space prompt"), "{out}");
}

/// Review: Space ist ohne etwas Aufklappbares ein No-op — ein kurzer
/// Prompt ohne Anker setzt `prompt_open` nicht, PgUp/PgDn bleiben bei der
/// Liste.
#[test]
fn c36_space_does_nothing_when_nothing_is_clipped() {
    let (_dir, repo, inspection) = without_claims("Write the files", ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(!out.contains("Space prompt"), "{out}");
    assert!(!out.contains("more lines"), "{out}");
    app.reduce(Action::Space);
    let state = app.overview.as_ref().unwrap();
    assert!(!state.prompt_open && !state.requirement_open);
}

/// Eine Quelle mit belegtem Anker — trifft erst nach dem ersten Space ein.
struct WithAnchor;

impl crate::Source for WithAnchor {
    fn load(&self) -> minds_reader::Result<Inspection> {
        unreachable!("this test never reloads")
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        None
    }
    fn intents(&self, named: &[minds_core::ContentHash]) -> Result<crate::IntentList, String> {
        let entries = named
            .iter()
            .map(|id| crate::IntentInfo {
                id: id.clone(),
                active: false,
                detail: Ok(crate::IntentDetail {
                    source: "file:docs/anforderung.md@abc".into(),
                    content: minds_core::ContentHash::from_bytes([2; 32]),
                    scope: Vec::new(),
                    version: None,
                    proof: Ok(()),
                    signature: minds_reader::assurance::IntentSignature::Unsigned,
                    snapshot_len: 1,
                    snapshot: Some(vec!["Sort stably.".into()]),
                    snapshot_clipped: false,
                }),
            })
            .collect::<Vec<_>>();
        Ok(crate::IntentList {
            total: entries.len(),
            entries,
            skipped: 0,
        })
    }
    fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
        Err("minds verify not found".into())
    }
}

/// Review: Space hält Anforderung und Prompt im Gleichschritt — auch wenn
/// der Anker erst nach dem ersten Space gelesen wird, schließt das
/// nächste Space beides; nie wechseln sich die Flags ab.
#[test]
fn c36_space_keeps_requirement_and_prompt_in_step() {
    // Ein langer Prompt und ein Anker an derselben Session.
    let mut s = session(&twenty_lines(), "2026-10-02T10:00:00Z");
    s.intent_events.push(IntentEvent {
        seq: 1,
        anchor_id: ContentHash::from_bytes([1; 32]),
        opens_session: true,
    });
    let (_dir, repo, inspection) = linked(vec![(sid('a'), s)]);
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    let flags = |app: &App| {
        let s = app.overview.as_ref().unwrap();
        (s.requirement_open, s.prompt_open)
    };
    // Gezeichnet (der Prompt ist gekürzt), der Anker noch nicht gelesen.
    let _ = detail_text(&mut app, 160, 40);
    app.reduce(Action::Space);
    assert_eq!(flags(&app), (false, true));
    while app.fill_overview(&WithAnchor) {}
    assert!(app.overview.as_ref().unwrap().has_requirement_text());
    app.reduce(Action::Space);
    assert_eq!(flags(&app), (false, false));
    let _ = detail_text(&mut app, 160, 40);
    app.reduce(Action::Space);
    assert_eq!(flags(&app), (true, true));
    let open = detail_text(&mut app, 160, 60);
    assert!(open.contains("Sort stably."), "{open}");
    assert!(open.contains("line 20 of the request"), "{open}");
    app.reduce(Action::Space);
    assert_eq!(flags(&app), (false, false));
}

/// Review: der Pfad mit mehreren Gründen und ohne Dateiclaim-Notiz — als
/// Komponententest über `coverage_lines_with`, wie c3v2 es für die
/// Klassen tut.
#[test]
fn c36_further_reasons_are_counted_and_no_claim_note_without_a_card() {
    use minds_reader::changes::{LineClasses, ReasonCount};
    let text = |c: &LineClasses, claimed: Option<usize>| {
        crate::view::overview::coverage_lines_with(Some(&Ok(c.clone())), claimed, 120)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let c = LineClasses {
        added: 9,
        unexplained: 5,
        files: 3,
        reasons: vec![
            ReasonCount {
                reason: "a tool claim writes the file without a hash",
                lines: 3,
            },
            ReasonCount {
                reason: "no tool claim or shell mention found",
                lines: 2,
            },
        ],
        ..LineClasses::default()
    };
    let out = text(&c, None);
    assert!(
        out.contains("— a tool claim writes the file without a hash: 3"),
        "{out}"
    );
    assert!(out.contains("+ 1 other reason → d"), "{out}");
    assert!(!out.contains("all 5"), "{out}");
    assert!(!out.contains("session records"), "{out}");
    assert!(!out.contains('✓'), "{out}");
    assert!(text(&c, Some(0)).contains("(session records 0 files)"));
    assert!(!text(&c, Some(2)).contains("session records"));
}

/// AC4: `unknown` ist ein Platzhalter, kein Modellname — das Detail sagt
/// „model not recorded"; ein bekanntes Modell steht mit Anbieter da.
#[test]
fn c36_unknown_is_never_printed_as_a_value() {
    let (_dir, repo, inspection) = without_claims("Do it", ("unknown", "unknown"));
    let mut app = on_head(&repo, inspection);
    let detail = detail_text(&mut app, 160, 40);
    assert!(!detail.contains("unknown"), "{detail}");
    assert!(detail.contains("model not recorded"), "{detail}");

    let (_dir, repo, inspection) = without_claims("Do it", ("anthropic", "claude-opus-4-1"));
    let mut app = on_head(&repo, inspection);
    let detail = detail_text(&mut app, 160, 40);
    assert!(
        detail.contains("claude-code · claude-opus-4-1 (Anthropic)"),
        "{detail}"
    );
    assert!(!detail.contains("not recorded"), "{detail}");
}

/// AC5: Mehrere Sessions — ein PROMPT je Session als Unterabschnitt ihrer
/// Karte, in Session-Reihenfolge; jeder Prompt steht genau einmal, keiner
/// davor als eigener Abschnitt (so scrollt er mit dem Fenster über die
/// Sessions, und die gewählte bleibt sichtbar).
#[test]
fn c36_several_sessions_get_a_prompt_each_in_order() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = opened(&repo, inspection);
    app.overview.as_mut().unwrap().select_commit(commits[2]);
    let detail = detail_text(&mut app, 160, 40);
    assert_eq!(detail.matches("PROMPT").count(), 2, "{detail}");
    assert_eq!(detail.matches("Fix retry handling").count(), 1, "{detail}");
    assert_eq!(detail.matches("Write the docs").count(), 1, "{detail}");
    let sessions = sessions_block(&detail);
    assert!(sessions.contains("SESSIONS (2)"), "{detail}");
    // Beide Prompts in den Karten, nach dem Kopf ihrer Session, in Reihenfolge.
    let a = sessions
        .find("b3-aaaaaaaa")
        .unwrap_or_else(|| panic!("{detail}"));
    let first = sessions
        .find("Fix retry handling")
        .unwrap_or_else(|| panic!("{detail}"));
    let c = sessions
        .find("b3-cccccccc")
        .unwrap_or_else(|| panic!("{detail}"));
    let second = sessions
        .find("Write the docs")
        .unwrap_or_else(|| panic!("{detail}"));
    assert!(a < first && first < c && c < second, "{detail}");
    let before = &detail[..detail.find("SESSIONS (").unwrap()];
    assert!(!before.contains("PROMPT"), "{detail}");
}
