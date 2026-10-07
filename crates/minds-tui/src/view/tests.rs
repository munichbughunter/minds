//! Render-Proben über `TestBackend`: Jede Ebene wird in einen Puffer
//! gezeichnet und auf die Zeilen geprüft, die ihre Aussage tragen — Leerzustand,
//! gefüllte Liste, degradierte Zeile, Suche, Graph in zwei Stufen, Why-Kette
//! mit fehlendem Glied, Inspector für eine Vermutung.
//!
//! Geprüft wird auf Teilstrings, nicht auf den ganzen Puffer: Das Layout darf
//! sich bewegen, die Aussage nicht.

use std::collections::BTreeMap;
use std::process::Command;

use minds_core::{
    Agent, Decision, Effect, EffectKind, EvidenceMark, EvidenceSource, Intent, Lineage, Model,
    Review, Role, Session, SessionId, Subject, ToolCall, Turn,
};
use minds_git::{CommitId, Repo};
use minds_reader::{Degradation, Degraded, Index, Inspection};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, View};
use crate::input::Action;

fn sid(c: char) -> SessionId {
    format!("b3-{}", c.to_string().repeat(64)).parse().unwrap()
}

fn commit(c: char) -> CommitId {
    c.to_string().repeat(40).parse().unwrap()
}

fn session(request: &str, started: &str) -> Session {
    let mut s = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "1".into(),
        },
        Model {
            provider: "anthropic".into(),
            id: "opus".into(),
        },
        Intent {
            request: request.into(),
            ..Intent::default()
        },
    );
    let mut l = Lineage::new("l");
    l.started_at = Some(started.into());
    s.lineage = Some(l);
    s.produced.files.push("src/http/retry.rs".into());
    s.turns.push(Turn {
        role: Role::Assistant,
        text: "Ich lese und ändere.".into(),
        tool_calls: vec![
            ToolCall {
                outcome: None,
                capture: None,
                name: "Read".into(),
                arguments: "{}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Read,
                    path: Some("src/http/retry.rs".into()),
                    content: None,
                    written: None,
                    written_unavailable: None,
                }),
            },
            ToolCall {
                outcome: None,
                capture: None,
                name: "Edit".into(),
                arguments: "{}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Write,
                    path: Some("src/http/retry.rs".into()),
                    content: None,
                    written: None,
                    written_unavailable: None,
                }),
            },
            ToolCall {
                outcome: None,
                capture: None,
                name: "Bash".into(),
                arguments: "{\"command\":\"cargo test\"}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Exec,
                    path: None,
                    content: None,
                    written: None,
                    written_unavailable: None,
                }),
            },
        ],
        parent: None,
        at: Some(started.into()),
    });
    s
}

/// Ein leeres Git-Repo — die Oberfläche fragt es nur im Inspector und beim
/// Blame, und beide Wege sind hier fail-soft.
fn repo() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["init", "-q"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git init");
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo)
}

/// Eine saubere, unsignierte Epoche für die Session `id`.
fn clean_seal(id: SessionId) -> (minds_core::ContentHash, minds_core::evidence::Seal) {
    let seal = minds_core::evidence::Seal {
        root: minds_core::ContentHash::from_bytes([9u8; 32]),
        agent: "claude-code".into(),
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
        first_seq: 0,
        last_seq: 3,
        events: 4,
        gaps: 0,
        pre_chain: 0,
        outcome: minds_core::evidence::SealOutcome::Stored {
            session: id.to_string(),
        },
        previous: None,
        last_event_at: "2026-07-25T14:10:00Z".into(),
    };
    let seal_id = minds_core::evidence::Seal::id_of_text(&seal.to_text().unwrap());
    (seal_id, seal)
}

fn filled() -> Inspection {
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(
        sid('b'),
        session("Add exponential backoff", "2026-07-25T13:41:00Z"),
    );
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    let change: minds_core::ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
    let mut changes = BTreeMap::new();
    changes.insert(commit('1'), change.clone());
    // Session a ist versiegelt (eine saubere Epoche) — Session b bewusst
    // nicht: Der Unterschied gehört zum Fixture, weil die Oberfläche ihn
    // aussprechen muss.
    let (seal_id, seal) = clean_seal(sid('a'));
    let index = Index::from_parts(sessions, commits)
        .with_changes(changes)
        .with_seals(sid('a'), vec![(seal_id, seal, false)])
        .with_degraded(vec![Degraded {
            id: sid('d'),
            cause: Degradation::Forgotten {
                reason: "DSGVO".into(),
            },
        }]);
    Inspection::from_index(
        index,
        vec![Review::new(
            Subject::Change(change.to_string()),
            Decision::NeedsWork,
            "pd",
            "Retry bei 5xx fehlt",
            Some("2026-07-26T00:00:00Z".into()),
        )],
        "payment-service",
    )
}

/// Das Referenz-Terminal: geteilt, mit einem Detail so breit wie früher der
/// ganze Bildschirm (208 · 40 % = 83 Liste, 1 Luft, 124 Detail) — die
/// Inhalts-Proben der Ebenen bleiben davon unberührt.
const WIDE: u16 = 208;
/// Geteilt, aber knapp: die Liste auf ihrer Untergrenze.
const SPLIT: u16 = 124;
/// Eine Fläche nach der anderen, wie vor der Teilung.
const NARROW: u16 = 100;

fn render(app: &mut App) -> String {
    render_at(app, WIDE)
}

fn render_at(app: &mut App, width: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal.backend().to_string()
}

/// Die Spalte der rechten Rahmenecke des SESSIONS-Blocks — die Grenze
/// zwischen Liste und Detail.
fn list_edge(out: &str) -> usize {
    out.lines()
        .find(|line| line.contains(" SESSIONS "))
        .and_then(|line| line.chars().position(|c| c == '┐'))
        .expect("SESSIONS block")
}

/// Die Listenspalte: alles links der Grenze — damit eine Probe sagen
/// kann, *wo* etwas steht.
fn list_of(out: &str) -> String {
    let edge = list_edge(out);
    out.lines()
        .map(|line| line.chars().take(edge + 1).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Das Detail: alles rechts der Grenze.
fn detail_of(out: &str) -> String {
    let edge = list_edge(out);
    out.lines()
        .map(|line| line.chars().skip(edge + 1).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_empty_state_points_to_enable() {
    let (_dir, repo) = repo();
    let mut app = App::new(Inspection::default(), &repo, None);
    let out = render(&mut app);
    assert!(out.contains("No sessions captured yet."), "{out}");
    assert!(out.contains("minds enable"), "{out}");
    assert!(out.contains("0 Sessions"), "{out}");
}

/// Die volle Tabelle — alle Spalten samt Umfang. Neben der Vorschau braucht
/// die Liste dafür ihre breiteste Stufe, also ein sehr breites Terminal.
#[test]
fn the_list_shows_newest_first_with_evidence_verdict_and_a_degraded_row() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = list_of(&render_at(&mut app, 300));
    assert!(out.contains("MINDS payment-service"), "{out}");
    // Die Tabelle traegt Spaltenkoepfe und Rahmen-Titel (Demo-Politur):
    // die Beweisspalten sind ohne Legende lesbar.
    assert!(out.contains(" SESSIONS "), "{out}");
    for header in ["TIME", "AGENT", "SIZE", "SEAL", "VERDICT"] {
        assert!(out.contains(header), "{header} fehlt: {out}");
    }
    assert!(out.contains("2 Sessions · 1 Changes"), "{out}");
    assert!(out.contains("1 degraded"), "{out}");
    let fix = out.find("Fix retry handling").unwrap();
    let backoff = out.find("Add exponential backoff").unwrap();
    let forgotten = out
        .find("forgotten: DSGVO")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(fix < backoff && backoff < forgotten, "{out}");
    // Die Verdikt-Spalte (ADR-0011): Session a ist versiegelt, b nicht.
    assert!(out.contains("◈ sealed"), "{out}");
    // Session b hat keine Seals: explizit LEGACY, kein leeres Nichts.
    assert!(out.contains("· legacy"), "{out}");
    // Der Kanten-Beleg als Glyph mit Status-Modifikator — beobachtet heisst
    // nicht geprueft.
    assert!(out.contains("● ?"), "{out}");
    assert!(out.contains("↻ needs work"), "{out}");
    assert!(out.contains("⌦ forgotten"), "{out}");
    assert!(out.contains("25.07. 14:10Z"), "{out}");
    assert!(out.contains("context coverage"), "{out}");
}

#[test]
fn the_search_filters_live_and_shows_its_chip() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::SearchStart);
    for c in "backoff".chars() {
        app.reduce(Action::SearchInput(c));
    }
    let out = render(&mut app);
    assert!(out.contains("/backoff"), "{out}");
    assert!(out.contains("1/3 match(es)"), "{out}");
    assert!(out.contains("Add exponential backoff"), "{out}");
    assert!(!out.contains("Fix retry handling"), "{out}");
    app.reduce(Action::SearchCommit);
    let out = render(&mut app);
    assert!(out.contains("[backoff]"), "{out}");
    app.reduce(Action::Back);
    assert!(app.query.is_empty());
    assert_eq!(app.visible.len(), 3);
    // No match ist ein Zustand, kein Fehler.
    app.reduce(Action::SearchStart);
    for c in "nirgends".chars() {
        app.reduce(Action::SearchInput(c));
    }
    let out = render(&mut app);
    assert!(out.contains("No match"), "{out}");
}

#[test]
fn enter_opens_the_graph_and_the_zoom_levels_fold_the_lane() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    assert!(matches!(app.top(), Some(View::Graph { .. })));
    let out = render(&mut app);
    assert!(out.contains("SESSION b3-aaaaaaaa…"), "{out}");
    assert!(out.contains(" YOU "), "{out}");
    assert!(out.contains("Fix retry handling"), "{out}");
    assert!(out.contains("◉ AGENT claude-code · opus"), "{out}");
    assert!(out.contains("◇ READ src/http/retry.rs"), "{out}");
    assert!(out.contains("✎ EDIT src/http/retry.rs"), "{out}");
    assert!(out.contains("▶ EXEC cargo test"), "{out}");
    assert!(out.contains("◆ CHANGE I"), "{out}");
    assert!(out.contains("↻ REVIEW needs work"), "{out}");
    assert!(out.contains("┣━"), "{out}");
    assert!(out.contains("┗━"), "{out}");
    // Keine Züge in der Normalstufe, in der ausführlichen schon.
    assert!(!out.contains("ASSISTANT"), "{out}");
    app.reduce(Action::Zoom(3));
    let out = render(&mut app);
    assert!(
        out.contains("· TURN ASSISTANT · Ich lese und ändere."),
        "{out}"
    );
    app.reduce(Action::Zoom(1));
    let out = render(&mut app);
    assert!(!out.contains("ASSISTANT"), "{out}");
    assert!(out.contains("Zoom 1"), "{out}");
    // Der Cursor auf einem Knoten zeigt dessen Details.
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("Tool       Read"), "{out}");
    app.reduce(Action::Back);
    assert!(app.top().is_none());
}

#[test]
fn the_timeline_is_the_same_rows_without_the_tree() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::ToggleTimeline);
    let out = render(&mut app);
    assert!(out.contains("TIMELINE"), "{out}");
    assert!(!out.contains("┣━"), "{out}");
    assert!(out.contains("25.07. 14:10Z"), "{out}");
}

#[test]
fn why_shows_the_chain_and_a_missing_link_is_named_not_hidden() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down); // „Add exponential backoff" — ohne Commit
    app.reduce(Action::Why);
    assert!(matches!(app.top(), Some(View::Why { .. })));
    let out = render(&mut app);
    // Session b ist is not sealed — das Glied traegt seit ADR-0011 eine
    // ehrliche Luecke (UnsealedRange), zusaetzlich zur fehlenden Bewertung.
    assert!(out.contains("⚠ SESSION"), "{out}");
    assert!(out.contains("✓ AGENT"), "{out}");
    assert!(out.contains("✓ INTENT"), "{out}");
    // Aussage != Beweis (ADR-0011): der Intent-Text traegt das CLAIM-Label,
    // die observed-Kanten daneben bleiben das einzige Beweismittel.
    assert!(
        out.contains("◌ CLAIM — as recorded, not verified evidence"),
        "{out}"
    );
    assert!(out.contains("Add exponential backoff"), "{out}");
    assert!(out.contains("✓ EVIDENCE"), "{out}");
    assert!(out.contains("no edge"), "{out}");
    assert!(out.contains("open"), "{out}");
    // Lücken sind First-Class: je Glied ✓/⚠, unten der Block mit Begründung.
    assert!(out.contains("⚠ REVIEW"), "{out}");
    assert!(out.contains(" 2 GAPS "), "{out}");
    assert!(out.contains("is not sealed"), "{out}");
    assert!(
        out.contains("No review — nobody has decided on this change."),
        "{out}"
    );
    assert!(out.contains("⚠ 2 gaps in the chain"), "{out}");
}

#[test]
fn a_fully_backed_chain_says_so_and_focus_explains_the_evidence_without_enter() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why); // „Fix retry handling" — Trailer, Change-Id, Review
    let out = render(&mut app);
    assert!(out.contains(" NO GAP "), "{out}");
    assert!(out.contains("✓ EVIDENCE"), "{out}");
    assert!(out.contains("✓ no gap"), "{out}");
    assert!(!out.contains("WHY IS THIS LINKED?"), "{out}");
    // Cursor auf das Evidence-Glied: Session, Agent, Intent, Evidence.
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    let out = render(&mut app);
    assert!(out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(out.contains("explicit provenance record"), "{out}");
    assert!(
        out.contains("carries the Minds-Session-Id trailer"),
        "{out}"
    );
}

#[test]
fn the_list_footer_says_what_the_focused_evidence_means() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render(&mut app);
    assert!(out.contains("Observed: the commit carries the"), "{out}");
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(
        out.contains("Unlinked: this session is attached to no commit"),
        "{out}"
    );
    app.reduce(Action::End);
    let out = render(&mut app);
    assert!(out.contains("Degraded:"), "{out}");
}

#[test]
fn a_change_node_in_the_graph_explains_its_proof() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::End);
    app.reduce(Action::Up); // Review → Change
    let out = render(&mut app);
    assert!(out.contains(" CHANGE "), "{out}");
    assert!(out.contains("Evidence   ● ? observed [unchecked]"), "{out}");
    assert!(out.contains("explicit provenance record"), "{out}");
}

#[test]
fn the_inspector_explains_the_focused_edge() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    // Zum Evidence-Glied: Session, Agent, Intent, Evidence — der Inspector
    // folgt dem Fokus, kein Enter nötig (#131).
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    let out = render(&mut app);
    assert!(out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(out.contains("● ? observed [unchecked]"), "{out}");
    assert!(
        out.contains("carries the Minds-Session-Id trailer"),
        "{out}"
    );
    // Der Satz kann am Zeilenumbruch brechen — kurzer, stabiler Anker.
    assert!(out.contains("Status: never"), "{out}");
    // Der Hinweis steht an der Kante und verspricht die echte Aktion.
    assert!(out.contains("Enter ↵ Commit"), "{out}");
    // Esc schließt den Inspector; weg vom Evidence-Glied bleibt er zu.
    app.reduce(Action::Back);
    app.reduce(Action::Up);
    let out = render(&mut app);
    assert!(!out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(matches!(app.top(), Some(View::Why { .. })));
}

#[test]
fn evidence_edges_are_focusable_and_enter_opens_the_commit() {
    // #132: ↑/↓ wandern erst über die Kanten des Evidence-Glieds, Enter auf
    // einer Kante springt in die Why-Kette genau dieses Commits.
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    commits.insert(commit('2'), vec![sid('a')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, commits), vec![], "repo");

    let (_dir, repo) = repo();
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Why);
    // Session, Agent, Intent → Evidence, Kante 0.
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    assert!(matches!(
        app.top(),
        Some(View::Why {
            cursor: 3,
            edge: 0,
            ..
        })
    ));
    // Down bleibt im Glied und rückt zur zweiten Kante vor …
    app.reduce(Action::Down);
    assert!(matches!(
        app.top(),
        Some(View::Why {
            cursor: 3,
            edge: 1,
            ..
        })
    ));
    // … erst das nächste Down verlässt es; Up kommt auf der letzten Kante an.
    app.reduce(Action::Down);
    assert!(matches!(
        app.top(),
        Some(View::Why {
            cursor: 4,
            edge: 0,
            ..
        })
    ));
    app.reduce(Action::Up);
    assert!(matches!(
        app.top(),
        Some(View::Why {
            cursor: 3,
            edge: 1,
            ..
        })
    ));

    // Enter auf Kante 1 → Why-Kette des zweiten Commits.
    app.reduce(Action::Enter);
    match app.top() {
        Some(View::Why { chain, .. }) => {
            assert!(
                matches!(
                    chain.steps.first(),
                    Some(minds_reader::model::WhyStep::Commit { id: Some(c), .. }) if *c == commit('2')
                ),
                "{:?}",
                chain.steps.first()
            );
        }
        other => panic!("{other:?}"),
    }
    // Esc trägt über den View-Stack zurück auf die fokussierte Kante.
    app.reduce(Action::Back);
    assert!(matches!(
        app.top(),
        Some(View::Why {
            cursor: 3,
            edge: 1,
            ..
        })
    ));
}

#[test]
fn an_inferred_edge_never_looks_like_an_observed_one() {
    // Glyph **und** Wort unterscheiden sich — nicht nur die Farbe, die in
    // einem monochromen Terminal verloren geht.
    let (glyph, word, _) =
        crate::theme::evidence(Some(EvidenceMark::of(EvidenceSource::Heuristic)));
    assert_eq!(glyph, "○ ?");
    assert_eq!(word, "inferred [unchecked]");
    let (glyph, word, _) = crate::theme::evidence(Some(EvidenceMark::of(EvidenceSource::Observed)));
    assert_eq!(glyph, "● ?");
    assert_eq!(word, "observed [unchecked]");

    // Und ein nachgerechneter Beleg unterscheidet sich vom ungeprüften —
    // wieder in Glyph UND Wort.
    let verified = EvidenceMark {
        source: EvidenceSource::Observed,
        status: minds_core::EvidenceStatus::Verified,
    };
    let (glyph, word, _) = crate::theme::evidence(Some(verified));
    assert_eq!(glyph, "● ✓");
    assert!(word.contains("recomputed"));
}

#[test]
fn evidence_mode_shows_the_verdict_and_the_detail_follows_focus() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Evidence); // Session a: eine saubere, unsignierte Epoche
    assert!(matches!(app.top(), Some(View::Evidence { cursor: 0, .. })));
    let out = render(&mut app);
    // Ebene 1: das Verdikt — und der Leitsatz, der die Grenze mitspricht.
    assert!(out.contains("EVIDENCE b3-aaaaaaaa…"), "{out}");
    // Die Seal-Karte: derselbe Wort-Stamm wie der CLI-Block aus
    // `minds checkpoint` — SESSION SEALED.
    assert!(out.contains(" SESSION SEALED "), "{out}");
    assert!(
        out.contains("4 event(s) · 0 gap(s) · 1 epoch(s) · 0/1 signed"),
        "{out}"
    );
    assert!(out.contains("◈ sealed"), "{out}");
    assert!(
        out.contains("Cryptographically verified within the recorded"),
        "{out}"
    );
    assert!(out.contains("INTEGRITY"), "{out}");
    assert!(
        out.contains("COMPLETE · 4 event(s) · within agent-hooks/v1"),
        "{out}"
    );
    assert!(out.contains("chain closed · 1 epoch(s)"), "{out}");
    // Verified ≠ signed: unsigniert ist ein eigener Zustand, kein ✗.
    assert!(out.contains("○"), "{out}");
    assert!(out.contains("NOT SIGNED — unsigned ≠ invalid"), "{out}");
    // Ebene 3 unter dem Fokus (INTEGRITY): die Kryptographie — samt der
    // ehrlichen Grenze des Proof-Modells.
    assert!(out.contains("blake3 · derive_key"), "{out}");
    assert!(
        out.contains("Chain root: reproducible only locally with journal + session salt"),
        "{out}"
    );

    // COVERAGE: Die Boundary ist prominent — „nicht erfasst" ist keine Lücke.
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("Observation boundary"), "{out}");
    assert!(out.contains("✓ agent hook events"), "{out}");
    assert!(out.contains("— network activity"), "{out}");
    assert!(out.contains("not captured, not a gap"), "{out}");
    assert!(
        out.contains("Missing evidence does not prove that nothing happened"),
        "{out}"
    );

    // ARTIFACT: Der Fixture-Commit existiert im leeren Repo nicht — der
    // Abgleich ist ein ehrlicher Zustand, kein Absturz und kein Fehlerrot.
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("ARTIFACT"), "{out}");
    assert!(out.contains("not assessed (error"), "{out}");

    // EPOCHEN: die Kette als Zeitleiste, nicht abstrakt.
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("Epoch 1/1"), "{out}");
    assert!(out.contains("#0–#3"), "{out}");
    assert!(out.contains("chain start"), "{out}");

    // SIGNATUR: unsigniert wird erklärt, nicht rot markiert.
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("○ NOT SIGNED"), "{out}");
    assert!(out.contains("nobody vouches for them with a key"), "{out}");
    assert!(out.contains("minds sign --seal"), "{out}");

    // GRENZEN: does_not_prove gehört in die Oberfläche, nicht nur in die Doku
    // — und zwar die der Stufe, die das Material trägt (EA-13).
    app.reduce(Action::End);
    let out = render(&mut app);
    assert!(
        out.contains("At A1 observed, Minds does NOT prove:"),
        "{out}"
    );
    assert!(
        out.contains("model name is what the agent reported"),
        "{out}"
    );

    app.reduce(Action::Back);
    assert!(app.top().is_none());
}

/// Snapshot der LIMITS-Sektion (EA-13): Die Fixture-Session trägt einen
/// sauberen `agent-hooks/v1`-Seal, also A1 — und genau die A1-Grenzen, samt
/// der append→seal-Lücke, in der Reihenfolge des Vokabulars. Eingefroren
/// als Text, unabhängig vom Layout.
#[test]
fn evidence_mode_limits_snapshot_a1() {
    let (_dir, repo) = repo();
    let app = App::new(filled(), &repo, None);
    let report = app.inspection.evidence_report(sid('a')).expect("Seal");
    let text: Vec<String> = super::evidence::limitations(&report)
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(
        text.join("\n"),
        "\
At A1 observed, Minds does NOT prove:
Level from the repository alone.

• Not which model produced the answers: the model name is what the agent reported.
• Not that the decision was right: the evidence shows what happened, not whether it was the correct thing to do.
• Not what happened outside the observation boundary: activity that neither the hooks nor the witness observe is not recorded.
• Not integrity against whoever controls the host (root, the witness account or its key): they can rewrite the evidence and the witness alike.
• Not that the record is complete: the hot path is fail-open, and a lost event is silently absent here (`minds fsck` makes gaps visible).
• Not that a session actually produced the lines attributed to it — the mapping comes from trailers (observed) and heuristics (inferred); the provenance is stated on every edge.
• Not that a model did what the transcript says — what is recorded is what the agent reported.
• Not that reported results (tests, builds) are real: they are what the agent reported until a CI replay reproduces them.
• Not who controls the signing keys. Without an allowed_signers file from a trusted source, a signature is only a self-attestation.
• Not that unsigned entries are genuine: they are content-addressed, but nobody vouches for them with a key.
• Not that the bundle alone can recompute the chain: the chain root is reproducible only with the local journal and session salt — the bundle proves the sealed claim (identity, signature, coverage), not the chain itself.
• Not the assurance level as a portable fact: it is assessed when the evidence is read, from the repository and the trusted signers at hand; a level stated elsewhere (an exported bundle, a report) cannot be recomputed from that document alone — re-run `minds verify --signers` against the repository.
• Not that nothing happened outside sealed ranges — a seal claims only the sequence range its epoch actually read.
• Not the integrity between append and seal: until the checkpoint, only the file system protects the journal; a local write before sealing is undetectable (ADR-0011, decision 1).
• Not that the agent process was the only actor: subprocesses, network access and plugins outside the hook boundary (scope in the seal) are not captured — coverage means complete within the boundary, never system activity.
• Not the effect of uninterpreted tool calls: capture=uninterpreted means observed, but the effects are not normalized — the interpretation axis is separate from integrity and coverage.
• Not real wall-clock time: timestamps come from the hook's local clock, with no external time anchor."
    );

    // Die Übersicht nennt dieselbe Stufe und Zahl.
    let mut app = app;
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(out.contains("17 named limits at A1 observed"), "{out}");
}

#[test]
fn evidence_mode_is_honest_about_a_legacy_session() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down); // Session b: keine Seals
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(out.contains("· legacy"), "{out}");
    assert!(out.contains(" SESSION · LEGACY "), "{out}");
    assert!(
        out.contains("cryptographic verification is not available"),
        "{out}"
    );
    assert!(out.contains("never gets a chain"), "{out}");
    // Keine Sektionen, kein Verdikt-Panel — kein leeres Gerüst.
    assert!(!out.contains("VERDICT"), "{out}");
}

#[test]
fn evidence_mode_opens_from_the_graph_too() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::Evidence);
    assert!(matches!(app.top(), Some(View::Evidence { .. })));
    // Esc führt in den Graphen zurück, nicht auf die Liste.
    app.reduce(Action::Back);
    assert!(matches!(app.top(), Some(View::Graph { .. })));
}

#[test]
fn the_help_overlays_and_closes() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Help);
    let out = render(&mut app);
    assert!(out.contains(" Help "), "{out}");
    assert!(out.contains("○ inferred"), "{out}");
    app.reduce(Action::Help);
    assert!(!app.help);
    app.reduce(Action::Quit);
    assert!(app.quit);
}

#[test]
fn why_line_in_an_empty_repo_ends_at_the_commit_not_in_an_error() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.open_why_line("src/http/retry.rs", 42).unwrap();
    let out = render(&mut app);
    assert!(out.contains("✓ LINE"), "{out}");
    assert!(out.contains("src/http/retry.rs:42"), "{out}");
    assert!(out.contains("Blame does not know this line"), "{out}");
}

#[test]
fn the_pipe_prints_tab_separated_lines_without_ansi() {
    let cards = filled().cards();
    let mut out = Vec::new();
    crate::pipe::cards(&mut out, &cards).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains('\u{1b}'));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let first: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(first.len(), 11, "{:?}", first);
    assert_eq!(first[0], "2026-07-25T14:10:00Z");
    assert_eq!(first[6], "observed [unchecked]");
    assert_eq!(first[7], "sealed");
    assert_eq!(first[8], "needs work");
    assert_eq!(first[10], "Fix retry handling");
    assert!(lines[2].contains("forgotten: DSGVO"));

    let chain = filled().why_commit(commit('1'));
    let mut out = Vec::new();
    crate::pipe::why(&mut out, &chain).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("commit\t1111111111"));
    assert!(text.contains("\nchange\tI"));
    assert!(text.contains("\nevidence\t"));
    assert!(text.contains("\nreview\tneeds work\n"));
    assert!(text.ends_with("review\tneeds work\n"), "{text}");

    let mut out = Vec::new();
    crate::pipe::why(&mut out, &filled().why_commit(commit('9'))).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("\ngap\tNoChangeId\t"), "{text}");
    assert!(text.contains("\ngap\tNoContext\t"), "{text}");
    assert!(
        text.ends_with("nobody has decided on this change.\n"),
        "{text}"
    );
}

#[test]
fn every_epistemic_state_differs_in_glyph_or_word_never_only_in_color() {
    // Die Farbschwäche-Regel, testfixiert: Jeder Zustand des Alphabets muss
    // sich in Glyph ODER Wort unterscheiden — Farbe trägt nie allein.
    use minds_core::{EvidenceMark as M, EvidenceSource as S, EvidenceStatus};
    use minds_reader::model::EvidenceVerdict;

    let mut seen = std::collections::BTreeSet::new();
    let mut check = |glyph: String, word: String| {
        assert!(
            seen.insert((glyph.clone(), word.clone())),
            "doppelt: {glyph:?} {word:?}"
        );
    };

    for source in [
        S::Heuristic,
        S::HumanDeclared,
        S::ContentDerived,
        S::Observed,
    ] {
        for status in [
            EvidenceStatus::Missing,
            EvidenceStatus::Unknown,
            EvidenceStatus::Partial,
            EvidenceStatus::Verified,
        ] {
            let (g, w, _) = crate::theme::evidence(Some(M { source, status }));
            check(g, w);
        }
    }
    let (g, w, _) = crate::theme::evidence(None);
    check(g, w);
    use minds_reader::model::{EvidenceState, Provenance};
    let chained = |verdict| {
        Provenance::Chained(EvidenceState {
            verdict,
            seals: 1,
            events: 1,
            gaps: 0,
            pre_chain: 0,
            rejected: false,
            chain_closed: true,
            signed: 0,
        })
    };
    for provenance in [
        chained(EvidenceVerdict::Verified),
        chained(EvidenceVerdict::Incomplete),
        chained(EvidenceVerdict::Tampered),
        Provenance::Legacy,
    ] {
        let (g, w, _) = crate::theme::provenance(&provenance);
        check(g.to_string(), w.to_string());
    }

    // Und die Uebergabe-Kante aus dem Evidence-DAG (ueber den Graph-Knoten).
    let (g, w, _) = crate::theme::node(&minds_reader::graph::NodeKind::Handover {
        other: sid('e'),
        incoming: true,
    });
    check(g.to_string(), w.to_string());
}

#[test]
fn an_uninterpreted_tool_call_shows_as_half_seen_not_as_a_plain_tool() {
    // Ein Agent ohne Adapter: Der Aufruf ist beobachtet, seine Wirkung nicht
    // gedeutet — ◐ statt ·, mit Wort (ADR-0011).
    let mut sessions = BTreeMap::new();
    let mut s = session("Wende den Patch an", "2026-07-25T14:10:00Z");
    s.turns[0].tool_calls = vec![ToolCall {
        outcome: None,
        capture: Some(minds_core::Capture {
            note: None,
            status: minds_core::CaptureStatus::Uninterpreted,
            adapter: "generic".into(),
            adapter_version: 1,
        }),
        name: "apply_patch".into(),
        arguments: r#"{"diff":"x"}"#.into(),
        effect: None,
    }];
    sessions.insert(sid('a'), s);
    let index = Index::from_parts(sessions, BTreeMap::new());
    let inspection = Inspection::from_index(index, vec![], "repo");

    let (_dir, repo) = repo();
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Enter);
    let out = render(&mut app);
    assert!(out.contains("◐"), "{out}");
    assert!(out.contains("OBSERVED"), "{out}");
}

/// Die Fußzeile isoliert — Badge-Assertions dürfen nicht versehentlich die
/// SEAL-Spalte der Tabelle matchen.
fn footer_of(out: &str) -> String {
    out.lines().rev().take(2).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_footer_badge_tracks_the_focused_session_and_view() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    // Liste: Karte a (versiegelt) fokussiert.
    let out = render(&mut app);
    assert!(footer_of(&out).contains("◈ sealed"), "{out}");
    // Naechste Karte: legacy — der Badge folgt dem Fokus.
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(footer_of(&out).contains("· legacy"), "{out}");
    assert!(!footer_of(&out).contains("◈ sealed"), "{out}");
    // Why: bewusst kein Badge (die Kette traegt mehrere Sessions).
    app.reduce(Action::Why);
    let out = render(&mut app);
    assert!(!footer_of(&out).contains("· legacy"), "{out}");
    assert!(!footer_of(&out).contains("◈ sealed"), "{out}");
    app.reduce(Action::Back);
    // Evidence: der Badge gehoert zur Karte hinter `id`, nicht zum Cursor.
    app.reduce(Action::Up);
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(footer_of(&out).contains("◈ sealed"), "{out}");
}

/// Der Demo-Moment, testfixiert: Eine manipulierte Session traegt den roten
/// Badge und die TAMPERED-Karte.
#[test]
fn a_tampered_session_shows_the_red_badge_and_card() {
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    let seal = minds_core::evidence::Seal {
        root: minds_core::ContentHash::from_bytes([9u8; 32]),
        agent: "claude-code".into(),
        scope: minds_core::evidence::SCOPE_AGENT_HOOKS_V1.into(),
        first_seq: 0,
        last_seq: 3,
        events: 4,
        gaps: 0,
        pre_chain: 0,
        outcome: minds_core::evidence::SealOutcome::Stored {
            session: sid('a').to_string(),
        },
        previous: None,
        last_event_at: "2026-07-25T14:10:00Z".into(),
    };
    let seal_id = minds_core::evidence::Seal::id_of_text(&seal.to_text().unwrap());
    let index = Index::from_parts(sessions, BTreeMap::new())
        .with_seals(sid('a'), vec![(seal_id, seal, false)])
        .with_tampered_seal(sid('a'));
    let inspection = Inspection::from_index(index, vec![], "repo");

    let (_dir, repo) = repo();
    let mut app = App::new(inspection, &repo, None);
    let out = render(&mut app);
    assert!(out.contains("✗ TAMPERED"), "{out}");
    assert!(footer_of(&out).contains("✗ TAMPERED"), "{out}");
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(out.contains(" SESSION TAMPERED "), "{out}");
    assert!(footer_of(&out).contains("✗ TAMPERED"), "{out}");
}

/// Das CLAIM-Label ist GESTYLT (theme::claim: HUMAN + DIM) — nie Default:
/// Eine Aussage, die aussieht wie der Text daneben, waere kein Label.
#[test]
fn the_claim_label_carries_its_theme_style() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let mut terminal = Terminal::new(TestBackend::new(124, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let cell = buffer
        .content()
        .iter()
        .find(|cell| cell.symbol() == "◌")
        .expect("CLAIM-Label sichtbar");
    let style = cell.style();
    assert_eq!(style.fg, Some(ratatui::style::Color::Cyan), "{style:?}");
    assert!(
        style.add_modifier.contains(ratatui::style::Modifier::DIM),
        "{style:?}"
    );
}

/// Unter der Teilung, aber ueber 97 Spalten: eine Flaeche, ohne SIZE — die
/// Beweisspalten bleiben.
#[test]
fn a_narrow_terminal_drops_the_size_column_but_keeps_the_evidence() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render_at(&mut app, NARROW);
    assert!(!out.contains("SIZE"), "{out}");
    for header in ["TIME", "AGENT", "SEAL", "VERDICT"] {
        assert!(out.contains(header), "{header} fehlt: {out}");
    }
    assert!(out.contains("◈ sealed"), "{out}");
}

// --- Die Teilung: Liste links, Detail rechts ------------------------------

/// Der Kern der Teilung: Liste und Graph der gewaehlten Karte im selben
/// Bild, ohne Enter — und die Vorschau wandert mit dem Cursor und dem Zoom.
#[test]
fn the_list_and_the_graph_preview_share_the_screen() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render_at(&mut app, SPLIT);
    assert!(app.top().is_none());
    let list = list_of(&out);
    let detail = detail_of(&out);
    assert!(list.contains(" SESSIONS "), "{out}");
    assert!(list.contains("Fix retry han"), "{out}");
    assert!(detail.contains("SESSION b3-aaaaaaaa…"), "{out}");
    assert!(detail.contains(" YOU "), "{out}");
    assert!(detail.contains("Fix retry handling"), "{out}");
    assert!(detail.contains("◇ READ src/http/retry.rs"), "{out}");
    // Die Vorschau ist nicht navigierbar: kein Detailkasten unter einem
    // Cursor, den es nicht gibt — und keine zweite invertierte Zeile neben
    // dem Listencursor.
    assert!(!detail.contains("Tool       Read"), "{out}");
    assert!(out.contains("Enter descend"), "{out}");
    let mut terminal = Terminal::new(TestBackend::new(SPLIT, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let edge = list_edge(&out) as u16;
    let reversed_in_detail = (0..buffer.area.height)
        .flat_map(|y| (edge + 1..buffer.area.width).map(move |x| (x, y)))
        .filter(|(_, y)| *y >= 2 && *y < buffer.area.height - 2) // Body, ohne Kopf/Fuss
        .any(|pos| {
            buffer[pos]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        });
    assert!(!reversed_in_detail, "{out}");
    // Cursor runter: die Vorschau folgt, der Stapel bleibt leer.
    app.reduce(Action::Down);
    let out = render_at(&mut app, SPLIT);
    assert!(app.top().is_none());
    assert!(out.contains("SESSION b3-bbbbbbbb…"), "{out}");
    assert!(out.contains("Add exponential backoff"), "{out}");
    assert!(!out.contains("SESSION b3-aaaaaaaa…"), "{out}");
    // Zoom auf der Liste wirkt auf die Vorschau wie auf eine Ebene.
    app.reduce(Action::Zoom(3));
    let out = render_at(&mut app, SPLIT);
    assert!(out.contains("TURN ASSISTANT"), "{out}");
    // Esc auf der Liste ist weiter „Suche loeschen, dann Ende" — die
    // Vorschau kostet keinen Tastendruck.
    app.reduce(Action::Back);
    assert!(app.quit);
}

/// Gelegte Ebenen — Graph, Why, Evidence — verdraengen die Liste nicht
/// mehr; sie bekommen die rechte Spalte.
#[test]
fn a_pushed_view_keeps_the_list_beside_it() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    assert!(matches!(app.top(), Some(View::Graph { .. })));
    let out = render_at(&mut app, SPLIT);
    assert!(list_of(&out).contains(" SESSIONS "), "{out}");
    let detail = detail_of(&out);
    assert!(detail.contains("◉ AGENT claude-code · opus"), "{out}");
    // Jetzt navigierbar: der Cursor traegt seinen Detailkasten.
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    let out = render_at(&mut app, SPLIT);
    assert!(detail_of(&out).contains("Tool       Read"), "{out}");
    app.reduce(Action::Why);
    assert!(matches!(app.top(), Some(View::Why { .. })));
    let out = render_at(&mut app, SPLIT);
    assert!(list_of(&out).contains(" SESSIONS "), "{out}");
    assert!(detail_of(&out).contains("✓ EVIDENCE"), "{out}");
    app.reduce(Action::Back);
    app.reduce(Action::Evidence);
    assert!(matches!(app.top(), Some(View::Evidence { .. })));
    let out = render_at(&mut app, SPLIT);
    assert!(list_of(&out).contains(" SESSIONS "), "{out}");
    assert!(detail_of(&out).contains("INTEGRITY"), "{out}");
    // Zurueck bis zur Liste: der Stapel leert sich wie vor der Teilung.
    app.reduce(Action::Back);
    app.reduce(Action::Back);
    assert!(app.top().is_none());
}

/// Unter der Teilungsbreite bleibt alles beim Alten: eine Flaeche, die
/// Liste **oder** die oberste Ebene.
#[test]
fn a_narrow_terminal_shows_one_pane_at_a_time() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    for width in [NARROW, 80] {
        let out = render_at(&mut app, width);
        assert!(out.contains(" SESSIONS "), "{width}: {out}");
        assert!(!out.contains("SESSION b3-"), "{width}: {out}");
        assert!(!out.contains(" YOU "), "{width}: {out}");
    }
    // Unter 97 Spalten nimmt auch die Einzelflaeche die kompakte Stufe —
    // frueher schnitt die Tabelle dort hart ab; der Seal-Befund bleibt.
    let out = render_at(&mut app, 80);
    assert!(out.contains("SEAL"), "{out}");
    assert!(out.contains("◈ sealed"), "{out}");
    assert!(!out.contains("VERDICT"), "{out}");
    app.reduce(Action::Enter);
    let out = render_at(&mut app, NARROW);
    assert!(out.contains("SESSION b3-aaaaaaaa…"), "{out}");
    assert!(!out.contains(" SESSIONS "), "{out}");
    app.reduce(Action::Back);
    assert!(app.top().is_none());
}

/// Eine degradierte Karte hat keinen Graphen: Die Vorschau sagt, warum —
/// mit demselben Satz wie die Fusszeile — statt zu panicken.
#[test]
fn a_degraded_card_previews_its_state_not_a_graph() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::End);
    assert!(app.selected().is_some_and(|c| c.is_degraded()));
    let out = render_at(&mut app, SPLIT);
    assert!(list_of(&out).contains("⌦ forgotten"), "{out}");
    let detail = detail_of(&out);
    assert!(
        detail.contains("Degraded: the payload is unreadable"),
        "{out}"
    );
    // Der Satz bricht um, statt am Spaltenrand abgeschnitten zu werden.
    assert!(detail.contains("the reference stays resolvable."), "{out}");
    assert!(!detail.contains(" YOU "), "{out}");
    assert!(footer_of(&out).contains("Degraded:"), "{out}");
    // Enter auf einer degradierten Karte legt weiterhin nichts.
    app.reduce(Action::Enter);
    assert!(app.top().is_none());
}

/// Ohne Karte bleibt das Detail leer — den Leerzustand sagt die Liste,
/// einmal, nicht zweimal.
#[test]
fn an_empty_list_leaves_the_preview_blank() {
    let (_dir, repo) = repo();
    let mut app = App::new(Inspection::default(), &repo, None);
    let out = render_at(&mut app, SPLIT);
    assert!(out.contains("No sessions captured yet."), "{out}");
    assert!(!out.contains(" YOU "), "{out}");
    assert!(!out.contains("SESSION b3-"), "{out}");
    assert_eq!(out.matches("minds enable").count(), 1, "{out}");

    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::SearchStart);
    for c in "nirgends".chars() {
        app.reduce(Action::SearchInput(c));
    }
    let out = render_at(&mut app, SPLIT);
    assert!(out.contains("No match"), "{out}");
    assert!(!out.contains(" YOU "), "{out}");
    assert!(!out.contains("SESSION b3-"), "{out}");
}

/// Neben der Vorschau ist die Liste kompakt: Der Manipulationsbefund
/// (SEAL) bleibt, Umfang und Review-Verdict weichen — der Graph-Kopf
/// daneben sagt beides. Mehr Breite gibt ihr die Spalten zurueck.
#[test]
fn the_list_column_keeps_the_seal_and_grows_with_the_terminal() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let list = list_of(&render_at(&mut app, SPLIT));
    for header in ["TIME", "SESSION", "AGENT", "SEAL"] {
        assert!(list.contains(header), "{header} fehlt: {list}");
    }
    assert!(!list.contains("VERDICT"), "{list}");
    assert!(!list.contains("SIZE"), "{list}");
    assert!(list.contains("◈ sealed"), "{list}");
    assert!(list.contains("· legacy"), "{list}");
    assert!(list.contains("● ?"), "{list}");
    assert!(list.contains("claude-code"), "{list}");
    // Ab der vollen Stufe kommt das Verdict zurueck, dann der Umfang.
    let list = list_of(&render_at(&mut app, 250));
    assert!(list.contains("VERDICT"), "{list}");
    assert!(list.contains("↻ needs work"), "{list}");
    assert!(!list.contains("SIZE"), "{list}");
    let list = list_of(&render_at(&mut app, 300));
    assert!(list.contains("SIZE"), "{list}");
}

/// Jeder Agent traegt seine Farbe in der AGENT-Spalte — und das Wort
/// daneben, ohne das die Farbe nichts sagte.
#[test]
fn the_agent_cell_carries_its_own_color() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let mut terminal = Terminal::new(TestBackend::new(SPLIT, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let expected = crate::theme::agent_color("claude-code · opus");
    // Die Zelle mit dem ersten `c` von „claude-code" in der Tabellenzeile
    // der Karte a — hinter dem Zeilen-Glyph, vor der SEAL-Spalte.
    let row = (0..buffer.area.height)
        .find(|y| {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, *y)].symbol().to_string())
                .collect();
            line.contains("Fix retry han")
        })
        .expect("Zeile der Karte a");
    let x = (0..buffer.area.width)
        .find(|x| {
            let line: String = (*x..(*x + 11).min(buffer.area.width))
                .map(|x| buffer[(x, row)].symbol().to_string())
                .collect();
            line == "claude-code"
        })
        .expect("AGENT-Zelle");
    assert_eq!(buffer[(x, row)].style().fg, Some(expected));
    // Und es ist wirklich eine eigene Farbe, nicht die alte Einheitsfarbe.
    assert_ne!(buffer[(x, row)].style().fg, Some(crate::theme::AGENT));
}

// ---------------------------------------------------------------------------
// EA-03: der Abgleich im Evidence-Mode, über die EA-01-Fixtures

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Ein `Write`-Aufruf samt Schreibzeit-Hash — wie EA-01a ihn speichert.
fn write_call(path: &str, text: &str) -> ToolCall {
    ToolCall {
        outcome: None,
        capture: None,
        name: "Write".into(),
        arguments: serde_json::json!({ "content": text }).to_string(),
        effect: Some(Effect {
            kind: EffectKind::Write,
            path: Some(path.into()),
            content: None,
            written: Some(minds_core::ContentHash::from_bytes(
                *blake3::hash(text.as_bytes()).as_bytes(),
            )),
            written_unavailable: None,
        }),
    }
}

/// Die EA-01-Fixtures als echter Commit: Die Session schreibt `a` und `b`;
/// mit `human` ändert danach ein Mensch Zeile 2 von `a`.
fn reconciled(human: bool) -> (tempfile::TempDir, Repo, Inspection) {
    let (dir, _) = repo();
    let a = if human {
        "one\nHUMAN\nthree\n"
    } else {
        "one\ntwo\nthree\n"
    };
    std::fs::write(dir.path().join("a"), a).unwrap();
    std::fs::write(dir.path().join("b"), "three\n").unwrap();
    git(dir.path(), &["add", "a", "b"]);
    git(dir.path(), &["commit", "-q", "-m", "write the files"]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();

    let mut s = session("Write the files", "2026-10-02T10:00:00Z");
    s.turns[0].tool_calls = vec![
        write_call("a", "one\ntwo\nthree\n"),
        write_call("b", "three\n"),
    ];
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

/// Der Puffer samt Stilen — für die Probe, dass Unerklärtes nie rot ist.
fn render_buffer(app: &mut App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(WIDE, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn evidence_mode_reconciles_a_human_line_as_not_observed() {
    let (_dir, repo, inspection) = reconciled(true);
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    // Die Verdikt-Zeile: Zusammenfassung und Commit.
    let row = out
        .lines()
        .find(|l| l.contains("ARTIFACT"))
        .expect("ARTIFACT row");
    assert!(row.contains("◦ ARTIFACT"), "{out}");
    assert!(row.contains("artifact 3/4 lines explained · "), "{out}");

    // Das Detail folgt dem Fokus: INTEGRITY → COVERAGE → ARTIFACT.
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    let out = render(&mut app);
    let detail = detail_of(&out);
    assert!(detail.contains(" ARTIFACT "), "{detail}");
    assert!(detail.contains("artifact 3/4 lines explained"), "{detail}");
    // Die Dateiliste, nach Pfad: a mit der menschlichen Zeile, b belegt.
    let file_lines: Vec<&str> = detail
        .lines()
        .map(|l| {
            // Der Rahmen des Detail-Blocks und das Zeilenende des Puffers.
            l.trim_start()
                .trim_start_matches('│')
                .trim_end_matches('"')
                .trim_end_matches('│')
                .trim_end()
        })
        .filter(|l| l.starts_with("  ◦ ") || l.starts_with("  ◇ "))
        .collect();
    assert_eq!(
        file_lines,
        [
            "  ◦ unexplained         a  line 2 not observed in the session",
            "  ◇ reported only       b",
        ],
        "{detail}"
    );
    assert!(detail.contains("◦ not observed in the session"), "{detail}");

    // Unerklärt ist nie ein Fehler: kein Rot, nirgends auf dem Schirm, und
    // die Marke trägt den neutralen Stil.
    let buffer = render_buffer(&mut app);
    let (_, _, neutral) = crate::theme::not_observed();
    let mut marks = 0;
    let alarming = [
        crate::theme::DELETE,
        crate::theme::REVIEW,
        ratatui::style::Color::Red,
        ratatui::style::Color::LightRed,
    ];
    for cell in buffer.content() {
        assert!(
            !alarming.contains(&cell.fg),
            "alarming cell {:?}",
            cell.symbol()
        );
        if cell.symbol() == "◦" {
            marks += 1;
            assert_eq!(Some(cell.fg), neutral.fg);
        }
    }
    assert!(marks >= 3, "row, file and legend carry the mark");
}

#[test]
fn evidence_mode_shows_an_agent_only_commit_as_fully_explained() {
    let (_dir, repo, inspection) = reconciled(false);
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    let row = out
        .lines()
        .find(|l| l.contains("ARTIFACT"))
        .expect("ARTIFACT row");
    // Belegt, aber nur berichtet: `◇`, nicht das grüne `✓` eines Zeugen.
    assert!(row.contains("◇ ARTIFACT"), "{out}");
    assert!(row.contains("artifact 4/4 lines explained"), "{out}");
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    let detail = detail_of(&render(&mut app));
    assert!(detail.contains("◇ reported only       a"), "{detail}");
    assert!(detail.contains("◇ reported only       b"), "{detail}");
    assert!(!detail.contains("◦ unexplained"), "{detail}");
}

#[test]
fn evidence_mode_without_a_commit_says_so() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down); // Session b: an keinem Commit
    let id = app.selected().unwrap().id;
    assert!(app.inspection.index().commits_of(id).is_empty());
    // Legacy zeigt keinen Abgleich ohne Commit — kein leeres Gerüst.
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(!out.contains("artifact "), "{out}");
}

#[test]
fn a_legacy_session_still_shows_its_reconciliation() {
    let (_dir, repo, inspection) = reconciled(true);
    // Dieselbe Fixture ohne Seal: Der Abgleich hängt nicht an der Chain.
    let index = inspection.index().clone();
    let mut sessions = BTreeMap::new();
    let mut commits = BTreeMap::new();
    for (id, session) in index.sessions() {
        sessions.insert(*id, session.clone());
        for commit in index.commits_of(*id) {
            commits.insert(commit, vec![*id]);
        }
    }
    let inspection = Inspection::from_index(Index::from_parts(sessions, commits), vec![], "r");
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::Evidence);
    let out = render(&mut app);
    assert!(out.contains(" SESSION · LEGACY "), "{out}");
    assert!(out.contains("artifact 3/4 lines explained"), "{out}");
    assert!(out.contains("line 2 not observed in the session"), "{out}");
}

#[test]
fn hostile_and_overlong_paths_are_sanitized_and_capped() {
    use minds_reader::artifact::{ArtifactState, Assessed, CommitArtifact};
    use minds_reader::reconcile::{FileRecon, LineLevel, ReconClass, Reconciliation};
    let long = format!("{}/x.rs", "d".repeat(100_000));
    let files = ["evil\u{202e}txt.exe\u{1b}[2J", long.as_str()]
        .into_iter()
        .map(|path| FileRecon {
            path: path.into(),
            class: ReconClass::Unexplained,
            line_level: LineLevel::Available(Vec::new()),
            committed: minds_core::ContentHash::from_bytes([0; 32]),
            deleted: false,
            changed_lines: 0,
            removes: false,
            last_observed: None,
        })
        .collect();
    let artifacts = vec![CommitArtifact {
        commit: commit('1'),
        subject: None,
        inferred: true,
        claimants: 1,
        state: ArtifactState::Assessed(Assessed {
            recon: Reconciliation {
                commit: commit('1'),
                base: None,
                files,
                explained_lines: 0,
                total_changed_lines: 0,
            },
            structural: Vec::new(),
        }),
    }];
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let report = app.inspection.evidence_report(sid('a'));
    app.views.push(View::Evidence {
        id: sid('a'),
        report,
        uninterpreted: 0,
        artifacts,
        cursor: 2,
    });
    let out = render(&mut app);
    assert!(!out.contains(['\u{1b}', '\u{202e}']), "{out}");
    assert!(
        out.contains("claims from inferred links (no trailer)"),
        "{out}"
    );
    // Gekappt und von vorn gekürzt: genau eine Zeile trägt den Pfad, und
    // sein Ende bleibt lesbar.
    let long_lines: Vec<&str> = out.lines().filter(|l| l.contains("dddd")).collect();
    assert_eq!(long_lines.len(), 1, "{out}");
    assert!(long_lines[0].contains("…dddd") && long_lines[0].contains("/x.rs"));
}

/// Nicht abgeglichen ist nicht „nicht beobachtet": Übersprungene Commits
/// ohne Unerklärtes tragen `·`, nicht das `◦` der Legende.
#[test]
fn skipped_commits_are_not_shown_as_not_observed() {
    use minds_reader::artifact::{ArtifactState, CommitArtifact};
    let skipped = |c: char| CommitArtifact {
        commit: commit(c),
        subject: None,
        inferred: false,
        claimants: 1,
        state: ArtifactState::Unavailable("skipped: too many linked commits"),
    };
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let report = app.inspection.evidence_report(sid('a'));
    app.views.push(View::Evidence {
        id: sid('a'),
        report,
        uninterpreted: 0,
        artifacts: vec![skipped('1'), skipped('2')],
        cursor: 0,
    });
    let out = render(&mut app);
    let row = out
        .lines()
        .find(|l| l.contains("ARTIFACT"))
        .expect("ARTIFACT row");
    assert!(row.contains("· ARTIFACT"), "{row}");
    assert!(row.contains("2 commits · 2 not assessed"), "{row}");
}
