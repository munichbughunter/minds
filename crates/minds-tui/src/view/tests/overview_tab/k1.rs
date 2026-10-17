//! K1 (Batch 2, Paket 1) · Der Grund für ein ungelesenes Transkript wird
//! angezeigt: Wo ein Modell stünde, steht `model not recorded — <Grund>`;
//! der Platzhalter `unknown` aus `minds-capture` erscheint in **keiner**
//! View (Klasse 1).

use minds_core::TranscriptStatus;
use minds_git::DiffKind;
use minds_reader::changes::{ChangeSet, DiffRow, FileDiff};
use minds_reader::model::ReviewState;
use minds_reader::reconcile::{LineSource, ReconClass};

use super::*;
use crate::changes::{ChangesState, Focus};

/// Eine Quelle ohne Anker und ohne Urteil.
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

/// Eine Session, deren Modell der Platzhalter ist und deren Lineage den
/// Grund trägt — wie die Demo-Session, die der geschützte Witness baute.
fn placeholder_session(status: Option<TranscriptStatus>) -> Session {
    let mut s = session("Implement the requirement", "2026-10-16T10:37:25Z");
    s.agent.version = "unknown".into();
    s.model = minds_core::Model {
        provider: "unknown".into(),
        id: "unknown".into(),
    };
    let mut lineage = Lineage::new("71f20f81");
    lineage.started_at = Some("2026-10-16T10:37:25Z".into());
    lineage.transcript = status;
    s.lineage = Some(lineage);
    s
}

/// Ein Repo mit einem Commit, an dem die Session hängt.
fn linked(s: Session) -> (tempfile::TempDir, Repo, CommitId, Inspection) {
    let (dir, _) = repo();
    std::fs::write(dir.path().join("src.py"), "one\ntwo\n").unwrap();
    git(dir.path(), &["add", "src.py"]);
    git(dir.path(), &["commit", "-q", "-m", "feat: stable sort"]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    let mut links = BTreeMap::new();
    links.insert(head, vec![sid('a')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, links), Vec::new(), "demo");
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo, head, inspection)
}

fn text_at(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ein Diff des Commits, dessen eine Zeile die Session `a` per Write
/// (Turn 1, Aufruf 2) gemeldet hat — die Why-Box zeigt dann den Agenten.
fn changes_of(head: CommitId) -> ChangesState {
    let row = |kind, old, new, text: &str, class, source| DiffRow {
        kind,
        old,
        new,
        text: text.into(),
        class,
        source,
    };
    let source = LineSource {
        session: sid('a'),
        turn: 0,
        call: 1,
    };
    let set = ChangeSet {
        commit: head,
        subject: Some("feat: stable sort".into()),
        change: None,
        review: ReviewState::open(),
        unassessed: None,
        files: vec![FileDiff {
            path: "src.py".into(),
            identity: "src.py".into(),
            added: 2,
            removed: 0,
            class: Some(ReconClass::ReportedOnly),
            gap: None,
            note: None,
            rows: vec![
                row(DiffKind::Hunk, None, None, "@@ -0,0 +1,2 @@", None, None),
                row(
                    DiffKind::Added,
                    None,
                    Some(1),
                    "one",
                    Some(ReconClass::ReportedOnly),
                    Some(source),
                ),
                row(
                    DiffKind::Added,
                    None,
                    Some(2),
                    "two",
                    Some(ReconClass::ReportedOnly),
                    Some(source),
                ),
            ],
        }],
    };
    let mut state = ChangesState::new(vec![head], 0, Ok(set));
    state.focus = Focus::Diff;
    state.row = 1;
    state.why = true;
    state
}

/// Jede View mit der Platzhalter-Session, als Text bei 160×40.
fn every_view(status: Option<TranscriptStatus>) -> Vec<(&'static str, String)> {
    let (_dir, repo, head, inspection) = linked(placeholder_session(status));
    let mut out = Vec::new();

    // Overview mit vollständig gelesenem Detail.
    let mut app = App::new(inspection.clone(), &repo, None);
    app.open_overview();
    while app.fill_overview(&Plain) {}
    out.push(("overview", text_at(&mut app, 160, 40)));

    // Sessions: Liste und Graph.
    let mut app = App::new(inspection.clone(), &repo, None);
    out.push(("sessions", text_at(&mut app, 160, 40)));
    app.reduce(Action::Enter);
    assert!(matches!(app.top(), Some(View::Story { .. })), "graph");
    out.push(("graph", text_at(&mut app, 160, 40)));

    // Verify, Changes (Why-Box), Intent, Hilfe.
    let mut app = App::new(inspection.clone(), &repo, None);
    app.reduce(Action::TabTo(2));
    out.push(("verify", text_at(&mut app, 160, 40)));
    let mut app = App::new(inspection.clone(), &repo, None);
    app.changes = Some(changes_of(head));
    app.tab = Tab::Changes;
    out.push(("changes", text_at(&mut app, 160, 40)));
    let mut app = App::new(inspection.clone(), &repo, None);
    app.reduce(Action::TabTo(4));
    out.push(("intent", text_at(&mut app, 160, 40)));
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Help);
    out.push(("help", text_at(&mut app, 160, 40)));
    out
}

/// Klasse 1: Das Wort `unknown` steht in keiner View — mit und ohne Grund.
#[test]
fn k1_the_placeholder_unknown_is_printed_in_no_view() {
    for status in [
        None,
        Some(TranscriptStatus::NotReadByWitness),
        Some(TranscriptStatus::NoPathInEvents),
        Some(TranscriptStatus::NotFound),
    ] {
        for (view, text) in every_view(status) {
            assert!(
                !text.contains("unknown"),
                "{view} with {status:?} prints the placeholder:\n{text}"
            );
        }
    }
}

/// Überall, wo das Modell stünde, steht der Grund: Overview-Detail, Why-Box
/// (WRITTEN BY / AGENT SAID), Graph-Kopf, Sessions-Liste.
#[test]
fn k1_every_model_site_names_why_the_transcript_was_not_read() {
    let reason = "model not recorded — the witness does not read transcripts from agent paths";
    let views = every_view(Some(TranscriptStatus::NotReadByWitness));
    let text = |name: &str| {
        views
            .iter()
            .find(|(view, _)| *view == name)
            .map(|(_, text)| text.clone())
            .unwrap()
    };
    // Overview: der Kopf behält Id und Zeit, der Grund steht in der Zeile
    // darunter — gedimmt, mit Gedankenstrich.
    let overview = text("overview");
    assert!(
        overview.contains("claude-code · model not recorded · b3-aaaaaaaa"),
        "{overview}"
    );
    assert!(
        overview.contains("    — the witness does not read transcripts from agent paths"),
        "{overview}"
    );
    // Why-Box in der Reihenfolge von C9.3: die Modell-Zeile unter WRITTEN BY
    // nennt den Grund; der Satz bricht in der Spalte um — der Anfang steht.
    let changes = text("changes");
    assert!(
        changes.contains("model not recorded — the witness"),
        "{changes}"
    );
    assert!(!changes.contains("Agent    "), "{changes}");
    // Der Graph-Knoten kürzt am Spaltenrand mit `…` — der Satz beginnt, und
    // `unknown` steht nirgends (siehe den Test davor).
    let starts = "model not recorded — the witness does not read";
    assert!(text("graph").contains(starts), "{}", text("graph"));
    let _ = reason;
    // Die Liste kürzt auf die Spalte — der Anfang steht.
    assert!(
        text("sessions").contains("claude-code · model not recorded"),
        "{}",
        text("sessions")
    );

    // Ein anderer Grund, ein anderer Satz — kein Satz ist hart verdrahtet.
    let views = every_view(Some(TranscriptStatus::NotFound));
    let overview = &views.iter().find(|(v, _)| *v == "overview").unwrap().1;
    assert!(
        overview.contains("    — the transcript file was not found at checkpoint time"),
        "{overview}"
    );

    // Ohne Status (altes Binary) bleibt es beim knappen Satz.
    let views = every_view(None);
    let overview = &views.iter().find(|(v, _)| *v == "overview").unwrap().1;
    assert!(overview.contains("model not recorded"), "{overview}");
    assert!(!overview.contains("    — the"), "{overview}");
}

/// Mit `Read` steht das Modell vollständig: `claude-opus-4-1 (Anthropic)`.
#[test]
fn k1_a_read_transcript_shows_the_model_in_full() {
    let mut s = session("Implement the requirement", "2026-10-16T10:37:25Z");
    s.model = minds_core::Model {
        provider: "anthropic".into(),
        id: "claude-opus-4-1".into(),
    };
    let mut lineage = Lineage::new("71f20f81");
    lineage.transcript = Some(TranscriptStatus::Read);
    s.lineage = Some(lineage);
    let (_dir, repo, _head, inspection) = linked(s);
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    while app.fill_overview(&Plain) {}
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("claude-code · claude-opus-4-1 (Anthropic)"),
        "{out}"
    );
    assert!(!out.contains("not recorded"), "{out}");
}

/// Fehlt der Text des Agenten, nennt AGENT SAID den Grund aus dem Record
/// (`— (transcript not read: <Grund>)`, C9.3) statt einer leeren Aussage.
#[test]
fn k1_agent_said_names_the_reason_when_the_transcript_was_not_read() {
    let mut s = placeholder_session(Some(TranscriptStatus::NotFound));
    for turn in &mut s.turns {
        if turn.role == minds_core::Role::Assistant {
            turn.text.clear();
        }
    }
    let (_dir, repo, head, inspection) = linked(s);
    let mut app = App::new(inspection, &repo, None);
    app.changes = Some(changes_of(head));
    app.tab = Tab::Changes;
    let out = text_at(&mut app, 160, 40);
    assert!(
        out.contains("(transcript not read: the transcript"),
        "{out}"
    );
    assert!(!out.contains("AGENT SAID   ◌"), "{out}");
    assert!(!out.contains("unknown"), "{out}");
}
