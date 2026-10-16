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

/// Ein echter Commit mit zwei Dateien (vier hinzugefügte Zeilen) und eine
/// Session, die keinen Dateiclaim trägt — wie der Screenshot vom 15.10.
fn without_claims(request: &str, model: (&str, &str)) -> (tempfile::TempDir, Repo, Inspection) {
    let (dir, _) = repo();
    std::fs::write(dir.path().join("a"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.path().join("b"), "four\n").unwrap();
    git(dir.path(), &["add", "a", "b"]);
    git(dir.path(), &["commit", "-q", "-m", "feat: two files"]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let mut s = session(request, "2026-10-02T10:00:00Z");
    s.produced.files.clear();
    s.turns[0].tool_calls.clear();
    s.model = minds_core::Model {
        provider: model.0.into(),
        id: model.1.into(),
    };
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    let mut commits = BTreeMap::new();
    commits.insert(head, vec![sid('a')]);
    let (seal_id, seal) = clean_seal(sid('a'));
    let index =
        Index::from_parts(sessions, commits).with_seals(sid('a'), vec![(seal_id, seal, false)]);
    let inspection = Inspection::from_index(index, vec![], "r");
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo, inspection)
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
    let detail = detail_text(&mut app, 160, 40);
    assert!(detail.contains("line 12 of the request"), "{detail}");
    assert!(!detail.contains("line 13 of the request"), "{detail}");
    assert!(detail.contains("… 8 more lines"), "{detail}");
    app.reduce(Action::Space);
    let open = detail_text(&mut app, 160, 40);
    assert!(open.contains("line 20 of the request"), "{open}");
    assert!(!open.contains("more lines"), "{open}");
    app.reduce(Action::Space);
    assert!(detail_text(&mut app, 160, 40).contains("… 8 more lines"));
}

/// AC1 bei 80 Spalten: Das Detail ist eine Ebene hinter Enter — und zeigt
/// den PROMPT ebenso.
#[test]
fn c36_the_prompt_is_there_in_a_narrow_terminal_too() {
    let (_dir, repo, inspection) = without_claims(&twenty_lines(), ("anthropic", "opus"));
    let mut app = on_head(&repo, inspection);
    let _ = lines_at(&mut app, 80, 24);
    app.reduce(Action::Enter);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("PROMPT"), "{out}");
    assert!(out.contains("line 01 of the request"), "{out}");
    assert!(out.contains("? help"), "{out}");
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
    assert!(detail.contains("→ d, then n"), "{detail}");
    // Eine Quelle: die Gründe summieren sich zu `◦n` des Changes-Tabs.
    let set = app.inspection.changes(&repo, commit).unwrap();
    let classes = set.line_classes();
    let sum: usize = classes.reasons.iter().map(|r| r.lines).sum();
    assert_eq!(sum, set.unexplained());
    assert_eq!(sum, 4);
    // Der Widerspruch steht in SESSIONS, als Lücke, nicht als Befund.
    let sessions = sessions_block(&detail);
    assert!(
        sessions.contains("0 files claimed · commit changes 2 files"),
        "{detail}"
    );
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut seen = false;
    for y in 0..40u16 {
        let row: String = (0..160u16).map(|x| buffer[(x, y)].symbol()).collect();
        if let Some(at) = row.find("files claimed") {
            seen = true;
            let x0 = u16::try_from(row[..at].chars().count()).unwrap();
            for x in x0..x0 + 13 {
                assert_ne!(buffer[(x, y)].fg, theme::DELETE, "a gap is not red: {row}");
            }
        }
    }
    assert!(seen);
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
