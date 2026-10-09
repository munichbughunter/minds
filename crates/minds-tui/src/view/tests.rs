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
            gap: None,
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

// --- Live neu laden -------------------------------------------------------

/// Ein Abdruck mit festem HEAD: `"2".into()` heißt „Refs anders, HEAD
/// gleich" — ein Neuladen von selbst ohne HEAD-Bewegung.
impl From<&str> for crate::Stamp {
    fn from(refs: &str) -> Self {
        Self {
            head: "HEAD main 1111".into(),
            refs: refs.into(),
        }
    }
}

/// Der Stand nach einer neuen, jüngeren Session `e` — sie steht danach oben.
fn grown() -> Inspection {
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(
        sid('b'),
        session("Add exponential backoff", "2026-07-25T13:41:00Z"),
    );
    sessions.insert(sid('e'), session("Live arrival", "2026-07-26T09:00:00Z"));
    let (seal_id, seal) = clean_seal(sid('a'));
    let index = Index::from_parts(sessions, BTreeMap::new())
        .with_seals(sid('a'), vec![(seal_id, seal, false)]);
    Inspection::from_index(index, Vec::new(), "payment-service")
}

/// Der Stand ohne Session `b` — etwa weil sie nicht mehr gelesen wird.
fn without_b() -> Inspection {
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    Inspection::from_index(
        Index::from_parts(sessions, BTreeMap::new()),
        Vec::new(),
        "payment-service",
    )
}

fn now() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(3723)
}

#[test]
fn reload_keeps_the_cursor_on_the_same_session() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down);
    assert_eq!(app.selected().map(|c| c.id), Some(sid('b')));
    app.reload(Ok(grown()), Some("2".into()), now());
    // Die neue Session schiebt b eine Zeile tiefer — der Cursor folgt der
    // Session, nicht der Zeilennummer.
    assert_eq!(app.selected().map(|c| c.id), Some(sid('b')));
    assert_eq!(app.cards.len(), 3);
    assert_eq!(app.stamp.as_ref().map(|s| s.refs.as_str()), Some("2"));
    assert_eq!(app.loaded_at, "01:02:03Z");
    let out = render(&mut app);
    assert!(out.contains("Live arrival"), "{out}");
}

#[test]
fn reload_keeps_the_search() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, Some("backoff".into()));
    assert_eq!(app.visible.len(), 1);
    app.reload(Ok(grown()), None, now());
    assert_eq!(app.query, "backoff");
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.visible_cards()[0].id, sid('b'));
}

#[test]
fn reload_rebuilds_the_open_stack_from_its_origins() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::Why);
    assert_eq!(app.views.len(), 2);
    app.reload(Ok(grown()), None, now());
    assert_eq!(app.views.len(), 2);
    assert!(matches!(&app.views[0], View::Graph { id, .. } if *id == sid('a')));
    assert!(matches!(
        &app.views[1],
        View::Why { origin: crate::app::WhyOrigin::Session(id), .. } if *id == sid('a')
    ));
}

#[test]
fn reload_keeps_the_evidence_section_under_the_cursor() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Evidence);
    app.reduce(Action::Down);
    app.reduce(Action::Down);
    app.reload(Ok(grown()), None, now());
    assert!(matches!(
        &app.views[..],
        [View::Evidence { id, cursor: 2, .. }] if *id == sid('a')
    ));
}

#[test]
fn a_vanished_session_drops_its_view_and_everything_above() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down);
    app.reduce(Action::Enter);
    app.reduce(Action::Evidence);
    assert_eq!(app.views.len(), 2);
    app.reload(Ok(without_b()), None, now());
    assert!(app.views.is_empty());
    assert_eq!(app.selected().map(|c| c.id), Some(sid('a')));
    // Der Sprung bleibt nicht unerklärt.
    let closed = app.closed.clone().expect("Grund vermerkt");
    assert!(closed.contains("is gone"), "{closed}");
    let out = render(&mut app);
    assert!(out.contains("⚠ closed session b3-bbbbbbbb"), "{out}");

    // Ein weiteres Neuladen ohne Taste nimmt den Hinweis nicht weg.
    app.reload(Ok(without_b()), None, now());
    assert!(app.closed.is_some());
}

#[test]
fn a_failed_reload_keeps_the_old_state_and_says_so() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    let before = app.loaded_at.clone();
    app.reduce(Action::Enter);
    app.reload(
        Err(minds_reader::ReaderError::UnbornHead),
        Some("2".into()),
        now(),
    );
    assert_eq!(app.cards.len(), filled().cards().len());
    assert_eq!(app.views.len(), 1);
    // Der Abdruck bleibt der alte: Die nächste Prüfung versucht es erneut.
    assert_eq!(app.stamp.as_ref().map(|s| s.refs.as_str()), Some("1"));
    assert_eq!(app.loaded_at, before);
    let out = render(&mut app);
    assert!(out.contains("⚠ reload failed, showing"), "{out}");
    assert!(out.contains("HEAD has no commit yet"), "{out}");
    // Der nächste Erfolg nimmt die Warnung zurück.
    app.reload(Ok(filled()), Some("2".into()), now());
    let out = render(&mut app);
    assert!(!out.contains("reload failed"), "{out}");
    assert!(out.contains("updated 01:02:03Z"), "{out}");
}

#[test]
fn r_asks_for_a_reload_but_not_while_typing_a_search() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Reload);
    assert!(app.reload_requested);
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::SearchStart);
    let r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    app.reduce(crate::input::map(r, app.searching));
    assert!(!app.reload_requested);
    assert_eq!(app.query, "r");
}

/// Eine Quelle, die mitzählt, wie oft geladen wurde — und auf Wunsch
/// scheitert.
struct Fake {
    stamp: std::cell::RefCell<Option<String>>,
    loads: std::cell::Cell<usize>,
    fail: std::cell::Cell<bool>,
}

impl Fake {
    fn at(stamp: &str) -> Self {
        Self {
            stamp: std::cell::RefCell::new(Some(stamp.into())),
            loads: std::cell::Cell::new(0),
            fail: std::cell::Cell::new(false),
        }
    }
}

impl crate::Source for Fake {
    fn load(&self) -> minds_reader::Result<Inspection> {
        self.loads.set(self.loads.get() + 1);
        if self.fail.get() {
            return Err(minds_reader::ReaderError::UnbornHead);
        }
        Ok(grown())
    }

    fn stamp(&self) -> Option<crate::Stamp> {
        self.stamp.borrow().as_deref().map(crate::Stamp::from)
    }
}

#[test]
fn refresh_loads_only_when_the_stamp_changes_or_r_was_pressed() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    let source = Fake::at("1");
    app.refresh(&source);
    assert_eq!(source.loads.get(), 0, "gleicher Abdruck: nichts laden");

    *source.stamp.borrow_mut() = Some("2".into());
    app.refresh(&source);
    assert_eq!(source.loads.get(), 1, "neuer Abdruck: laden");
    assert_eq!(app.stamp.as_ref().map(|s| s.refs.as_str()), Some("2"));
    assert_eq!(app.cards.len(), 3);

    *source.stamp.borrow_mut() = None;
    app.refresh(&source);
    assert_eq!(
        source.loads.get(),
        1,
        "unbestimmbar: nicht von selbst laden"
    );

    app.reduce(Action::Reload);
    app.refresh(&source);
    assert_eq!(source.loads.get(), 2, "r lädt immer");
    assert!(!app.reload_requested);
}

/// Ein dauerhaft scheiterndes Laden darf die Schleife nicht jede Sekunde
/// blockieren: Derselbe Abdruck wird nicht erneut versucht.
#[test]
fn a_failing_stamp_is_not_retried_in_a_hot_loop() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    let source = Fake::at("2");
    source.fail.set(true);
    for _ in 0..5 {
        app.refresh(&source);
    }
    assert_eq!(source.loads.get(), 1, "einmal versucht, dann Ruhe");
    assert!(app.reload_error.is_some());

    // Eine neue Änderung versucht es wieder — erfolgreich.
    source.fail.set(false);
    *source.stamp.borrow_mut() = Some("3".into());
    app.refresh(&source);
    assert_eq!(source.loads.get(), 2);
    assert!(app.reload_error.is_none());
    assert_eq!(app.stamp.as_ref().map(|s| s.refs.as_str()), Some("3"));
}

#[test]
fn r_retries_a_failed_stamp() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let source = Fake::at("2");
    source.fail.set(true);
    app.refresh(&source);
    app.refresh(&source);
    assert_eq!(source.loads.get(), 1);
    app.reduce(Action::Reload);
    app.refresh(&source);
    assert_eq!(
        source.loads.get(),
        2,
        "r lädt auch einen gescheiterten Abdruck"
    );
}

/// Kehrt der Stand zum geladenen zurück, stimmt das Bild wieder — die
/// Warnung darf nicht stehen bleiben.
#[test]
fn a_stale_failure_clears_when_the_state_returns() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    let source = Fake::at("2");
    source.fail.set(true);
    app.refresh(&source);
    assert!(app.reload_error.is_some());
    *source.stamp.borrow_mut() = Some("1".into());
    app.refresh(&source);
    assert!(app.reload_error.is_none());
    assert_eq!(source.loads.get(), 1, "kein Laden nötig");
}

#[test]
fn without_a_stamp_the_footer_says_live_is_off() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let source = Fake::at("1");
    *source.stamp.borrow_mut() = None;
    app.refresh(&source);
    assert!(!app.live);
    let out = render(&mut app);
    assert!(out.contains("⚠ live off — r reloads"), "{out}");
    *source.stamp.borrow_mut() = Some("1".into());
    app.refresh(&source);
    assert!(app.live);
    let out = render(&mut app);
    assert!(out.contains("· r reload"), "{out}");
}

/// Die Meldung kann Text aus dem Repository tragen: Bidi-, Zeilentrenner,
/// Tag-Zeichen und ANSI erreichen das Terminal nicht roh.
#[test]
fn a_reload_error_is_sanitized_for_the_footer() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let nasty = "a\u{202E}b\u{2028}c\u{E0041}d\u{1b}[2J";
    let err = minds_reader::ReaderError::Io {
        op: "read",
        path: nasty.into(),
        source: std::io::Error::other("x"),
    };
    app.reload(Err(err), Some("2".into()), now());
    let line = app.reload_error.clone().unwrap();
    for raw in ['\u{202E}', '\u{2028}', '\u{E0041}', '\u{1b}'] {
        assert!(!line.contains(raw), "{raw:?} roh in {line:?}");
    }
}

/// Neu laden rechnet wirklich neu: Session b ist erst Legacy, nach dem
/// Neuladen versiegelt — die offene Evidence-Ebene zeigt den neuen Stand.
#[test]
fn reload_recomputes_the_content_of_an_open_view() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Down);
    app.reduce(Action::Evidence);
    assert!(matches!(
        &app.views[..],
        [View::Evidence { report: None, .. }]
    ));
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('b'),
        session("Add exponential backoff", "2026-07-25T13:41:00Z"),
    );
    let (seal_id, seal) = clean_seal(sid('b'));
    let sealed = Inspection::from_index(
        Index::from_parts(sessions, BTreeMap::new())
            .with_seals(sid('b'), vec![(seal_id, seal, false)]),
        Vec::new(),
        "payment-service",
    );
    app.reload(Ok(sealed), None, now());
    assert!(matches!(
        &app.views[..],
        [View::Evidence { id, report: Some(_), .. }] if *id == sid('b')
    ));
}

#[test]
fn reload_rebuilds_a_commit_chain_from_the_new_model() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let chain = app.inspection.why_commit(commit('1'));
    app.views.push(View::Why {
        origin: crate::app::WhyOrigin::Commit(commit('1')),
        chain,
        cursor: 0,
        edge: 0,
        inspector: None,
    });
    // In `grown` trägt Commit 1 keine Session mehr.
    let next = grown();
    let expected = format!("{:?}", next.why_commit(commit('1')));
    app.reload(Ok(next), None, now());
    match &app.views[..] {
        [View::Why { origin, chain, .. }] => {
            assert_eq!(*origin, crate::app::WhyOrigin::Commit(commit('1')));
            assert_eq!(format!("{chain:?}"), expected);
        }
        other => panic!("{}", other.len()),
    }
}

/// Lässt sich eine Zeilen-Kette nicht mehr rechnen (HEAD mitten in einem
/// Checkout unlesbar), schließt sie — und die Fußzeile sagt, was und warum.
#[test]
fn a_line_chain_that_cannot_be_recomputed_closes_with_a_reason() {
    let (dir, repo) = repo();
    std::fs::write(dir.path().join(".git/HEAD"), "kein ref\n").unwrap();
    let mut app = App::new(filled(), &repo, None);
    app.views.push(View::Why {
        origin: crate::app::WhyOrigin::Line {
            path: "src/http/retry.rs".into(),
            line: 3,
        },
        chain: app.inspection.why_commit(commit('1')),
        cursor: 0,
        edge: 0,
        inspector: None,
    });
    app.reload(Ok(filled()), None, now());
    assert!(app.views.is_empty());
    let closed = app.closed.clone().expect("Grund vermerkt");
    assert!(closed.contains("why src/http/retry.rs:3"), "{closed}");
    let out = render(&mut app);
    assert!(out.contains("⚠ closed why src/http/retry.rs:3"), "{out}");
}

#[test]
fn the_footer_says_how_fresh_the_view_is_and_help_lists_r() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.loaded_at = crate::app::clock(now());
    let out = render(&mut app);
    assert!(out.contains("updated 01:02:03Z · r reload"), "{out}");
    app.reduce(Action::Help);
    let out = render(&mut app);
    assert!(out.contains("reload now"), "{out}");
}

/// Die Warnung steht vor den Tasten: Auch auf 80 Spalten ist sie zu lesen,
/// auf der Liste wie in einer Ebene und beim Tippen einer Suche.
#[test]
fn the_freshness_warning_survives_narrow_terminals() {
    let (_dir, repo) = repo();
    for width in [80, 120] {
        let mut app = App::new(filled(), &repo, None);
        app.reload(
            Err(minds_reader::ReaderError::UnbornHead),
            Some("2".into()),
            now(),
        );
        assert!(
            render_at(&mut app, width).contains("⚠ reload failed"),
            "{width}"
        );
        app.reduce(Action::Enter);
        assert!(
            render_at(&mut app, width).contains("⚠ reload failed"),
            "{width} Graph"
        );
        app.reduce(Action::Back);
        app.reduce(Action::SearchStart);
        assert!(
            render_at(&mut app, width).contains("⚠ reload failed"),
            "{width} Suche"
        );

        let mut app = App::new(filled(), &repo, None);
        app.live = false;
        assert!(render_at(&mut app, width).contains("⚠ live off"), "{width}");
    }
}

/// Scheitert das Laden und fehlt danach auch der Abdruck, sagt die Fußzeile
/// beides — sonst bliebe verborgen, dass nichts mehr von selbst lädt.
#[test]
fn a_failure_and_live_off_are_shown_together() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reload(
        Err(minds_reader::ReaderError::UnbornHead),
        Some("2".into()),
        now(),
    );
    let source = Fake::at("2");
    *source.stamp.borrow_mut() = None;
    app.refresh(&source);
    let out = render(&mut app);
    assert!(out.contains("⚠ reload failed · live off"), "{out}");
}

#[test]
fn the_closed_note_goes_away_with_the_next_key() {
    let (dir, repo) = repo();
    std::fs::write(dir.path().join(".git/HEAD"), "kein ref\n").unwrap();
    let mut app = App::new(filled(), &repo, None);
    app.views.push(View::Why {
        origin: crate::app::WhyOrigin::Line {
            path: "a.rs".into(),
            line: 1,
        },
        chain: app.inspection.why_commit(commit('1')),
        cursor: 0,
        edge: 0,
        inspector: None,
    });
    app.reload(Ok(filled()), None, now());
    assert!(app.closed.is_some());
    app.reduce(Action::Reload);
    assert!(app.closed.is_some(), "r allein liest den Hinweis nicht");
    app.reduce(Action::Down);
    assert!(app.closed.is_none());
    assert!(!render(&mut app).contains("closed"));
}

#[test]
fn the_clock_is_utc_and_wraps_at_midnight() {
    let at = |secs| crate::app::clock(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
    assert_eq!(at(0), "00:00:00Z");
    assert_eq!(at(86_399), "23:59:59Z");
    assert_eq!(at(86_400 + 61), "00:01:01Z");
}

/// Ein Platzhalter-Abgleich, den kein echtes Rechnen erzeugt — bleibt er
/// nach dem Neuladen stehen, wurde nicht neu gerechnet (kein Unterprozess).
fn marker() -> minds_reader::artifact::CommitArtifact {
    minds_reader::artifact::CommitArtifact {
        commit: commit('9'),
        subject: None,
        inferred: false,
        claimants: 1,
        state: minds_reader::artifact::ArtifactState::Unavailable("marker"),
    }
}

fn evidence_with_marker(app: &mut App) {
    let report = app.inspection.evidence_report(sid('a'));
    app.views.push(View::Evidence {
        id: sid('a'),
        report,
        uninterpreted: 0,
        artifacts: vec![marker()],
        cursor: 0,
    });
}

fn has_marker(app: &App) -> bool {
    matches!(&app.views[..], [View::Evidence { artifacts, .. }]
        if artifacts.iter().any(|a| a.commit == commit('9')))
}

/// Schreibt jemand nur Refs (HEAD steht, Commits und Claimants gleich),
/// rechnet ein Neuladen den Abgleich nicht neu — sonst bestimmte der
/// Schreiber, wie oft auf dem Host Unterprozesse laufen.
#[test]
fn an_automatic_reload_keeps_the_assessment_while_its_inputs_stand() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    evidence_with_marker(&mut app);
    app.reload(Ok(filled()), Some("2".into()), now());
    assert!(has_marker(&app), "übernommen");
}

#[test]
fn changed_inputs_or_r_recompute_the_assessment() {
    let (_dir, repo) = repo();
    // Andere Commits (wie nach einem Amend): neu gerechnet.
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    evidence_with_marker(&mut app);
    app.reload(Ok(grown()), Some("2".into()), now());
    assert!(!has_marker(&app), "Eingaben anders");

    // `r` (setzt `full_next`, siehe `r_sets_a_full_reload`): alles neu,
    // auch bei gleichen Eingaben.
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    evidence_with_marker(&mut app);
    app.full_next = true;
    app.reload(Ok(filled()), Some("1".into()), now());
    assert!(!has_marker(&app), "r rechnet neu");

    // HEAD bewegt: neu.
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    evidence_with_marker(&mut app);
    let moved = crate::Stamp {
        head: "HEAD main 2222".into(),
        refs: "1".into(),
    };
    app.reload(Ok(filled()), Some(moved), now());
    assert!(!has_marker(&app), "HEAD bewegt");
}

#[test]
fn r_sets_a_full_reload() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let source = Fake::at("1");
    app.stamp = Some("1".into());
    app.reduce(Action::Reload);
    app.refresh(&source);
    assert_eq!(source.loads.get(), 1);
    assert!(!app.full_next, "verbraucht");
}

/// Scheitert das von `r` verlangte Laden, rechnet das nächste gelungene
/// trotzdem alles neu — das Versprechen von `r` geht nicht verloren.
#[test]
fn a_failed_r_keeps_the_full_reload_for_the_next_success() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    evidence_with_marker(&mut app);
    app.full_next = true;
    app.reload(
        Err(minds_reader::ReaderError::UnbornHead),
        Some("1".into()),
        now(),
    );
    assert!(app.full_next);
    app.reload(Ok(filled()), Some("2".into()), now());
    assert!(!has_marker(&app), "voll neu gerechnet");
}

/// Ohne HEAD-Bewegung läuft für eine Zeilen-Kette kein Blame: Selbst ein
/// gerade unlesbares HEAD schließt sie nicht.
#[test]
fn a_line_chain_is_not_blamed_again_while_head_stands() {
    let (dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.stamp = Some("1".into());
    let mut chain = app.inspection.why_commit(commit('1'));
    chain.steps.insert(
        0,
        minds_reader::model::WhyStep::Line {
            path: "src/http/retry.rs".into(),
            line: 3,
        },
    );
    app.views.push(View::Why {
        origin: crate::app::WhyOrigin::Line {
            path: "src/http/retry.rs".into(),
            line: 3,
        },
        chain,
        cursor: 0,
        edge: 0,
        inspector: None,
    });
    std::fs::write(dir.path().join(".git/HEAD"), "kein ref\n").unwrap();
    app.reload(Ok(filled()), Some("2".into()), now());
    assert!(app.closed.is_none(), "{:?}", app.closed);
    assert!(matches!(&app.views[..], [View::Why { chain, .. }]
        if matches!(chain.steps.first(), Some(minds_reader::model::WhyStep::Line { .. }))));
    // `r` blamt neu — und scheitert jetzt sichtbar.
    app.full_next = true;
    app.reload(Ok(filled()), Some("2".into()), now());
    assert!(app.views.is_empty());
    assert!(app.closed.is_some());
}

#[test]
fn a_vanished_selection_moves_to_its_neighbour() {
    let (_dir, repo) = repo();
    let mut app = App::new(grown(), &repo, None);
    // Reihenfolge: e (neu), a, b — Cursor auf b, die letzte Zeile.
    app.reduce(Action::End);
    assert_eq!(app.selected().map(|c| c.id), Some(sid('b')));
    app.reload(Ok(without_b()), None, now());
    assert_eq!(app.selected().map(|c| c.id), Some(sid('a')), "Nachbar");
}

#[test]
fn r_works_while_the_help_is_open() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Help);
    app.reduce(Action::Reload);
    assert!(app.reload_requested);
    assert!(app.help, "die Hilfe bleibt offen");
}

#[test]
fn the_search_footer_does_not_promise_r() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::SearchStart);
    let out = render(&mut app);
    assert!(out.contains("updated"), "{out}");
    assert!(!out.contains("r reload"), "{out}");
    app.live = false;
    let out = render(&mut app);
    assert!(out.contains("⚠ live off"), "{out}");
    assert!(!out.contains("r reloads"), "{out}");
}

#[test]
fn a_degraded_session_closes_its_chain_with_the_right_reason() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.views.push(View::Why {
        origin: crate::app::WhyOrigin::Session(sid('d')),
        chain: app.inspection.why_commit(commit('1')),
        cursor: 0,
        edge: 0,
        inspector: None,
    });
    app.reload(Ok(filled()), None, now());
    assert!(app.views.is_empty());
    let closed = app.closed.clone().unwrap_or_default();
    assert!(closed.contains("has no why chain"), "{closed}");
}

// --- Changes-Tab ------------------------------------------------------------

mod changes_tab {
    use minds_git::DiffKind;
    use minds_reader::changes::{ChangeSet, DiffRow, FileDiff};
    use minds_reader::model::ReviewState;
    use minds_reader::reconcile::{LineSource, ReconClass};

    use super::*;
    use crate::app::Tab;
    use crate::changes::{ChangesState, Focus};

    fn row(
        kind: DiffKind,
        old: Option<u32>,
        new: Option<u32>,
        text: &str,
        class: Option<ReconClass>,
        source: Option<LineSource>,
    ) -> DiffRow {
        DiffRow {
            kind,
            old,
            new,
            text: text.into(),
            class,
            source,
        }
    }

    /// Ein Commit mit zwei Dateien: In `src/sort/mod.rs` schrieb die Session
    /// `a` eine Zeile (Edit, Turn 0, Aufruf 1), eine stammt von niemandem;
    /// `tests/t.rs` ist ganz unerklärt.
    fn set() -> ChangeSet {
        let source = LineSource {
            session: sid('a'),
            turn: 0,
            call: 1,
        };
        ChangeSet {
            commit: commit('1'),
            subject: Some("feat(sort): chronologisch".into()),
            change: None,
            review: ReviewState::open(),
            unassessed: None,
            files: vec![
                FileDiff {
                    path: "src/sort/mod.rs".into(),
                    identity: "src/sort/mod.rs".into(),
                    added: 2,
                    removed: 1,
                    class: Some(ReconClass::ReportedOnly),
                    gap: None,
                    note: None,
                    rows: vec![
                        row(DiffKind::Hunk, None, None, "@@ -1,3 +1,4 @@", None, None),
                        row(
                            DiffKind::Context,
                            Some(1),
                            Some(1),
                            "fn order() {",
                            None,
                            None,
                        ),
                        row(DiffKind::Removed, Some(2), None, "    old();", None, None),
                        row(
                            DiffKind::Added,
                            None,
                            Some(2),
                            "    sort_by_key();",
                            Some(ReconClass::ReportedOnly),
                            Some(source),
                        ),
                        row(
                            DiffKind::Added,
                            None,
                            Some(3),
                            "    // manuell nachgezogen",
                            Some(ReconClass::Unexplained),
                            None,
                        ),
                        row(DiffKind::Context, Some(3), Some(4), "}", None, None),
                    ],
                },
                FileDiff {
                    path: "tests/t.rs".into(),
                    identity: "tests/t.rs".into(),
                    added: 1,
                    removed: 0,
                    class: Some(ReconClass::Unexplained),
                    gap: None,
                    note: None,
                    rows: vec![
                        row(DiffKind::Hunk, None, None, "@@ -0,0 +1 @@", None, None),
                        row(
                            DiffKind::Added,
                            None,
                            Some(1),
                            "#[test] fn t() {}",
                            Some(ReconClass::Unexplained),
                            None,
                        ),
                    ],
                },
            ],
        }
    }

    fn app_with_changes(repo: &Repo) -> App<'_> {
        let mut app = App::new(filled(), repo, None);
        app.changes = Some(ChangesState::new(vec![commit('1')], 0, Ok(set())));
        app.tab = Tab::Changes;
        app
    }

    #[test]
    fn the_header_shows_the_tabs_and_tab_switches() {
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        let out = render(&mut app);
        assert!(out.contains("F1 Sessions"), "{out}");
        assert!(out.contains("F2 Verify"), "{out}");
        assert!(out.contains("F3 Changes"), "{out}");
        app.reduce(Action::CycleTab(true));
        assert_eq!(app.tab, Tab::Verify);
        // Ohne lesbaren Commit (leeres Repo) öffnet der Tab trotzdem — mit
        // einer ehrlichen Meldung statt eines Absturzes.
        app.reduce(Action::TabTo(2));
        assert_eq!(app.tab, Tab::Changes);
        assert!(render(&mut app).contains("Changes unavailable") || app.changes.is_some());
        assert!(out.contains("F4 Intent"), "{out}");
        app.reduce(Action::TabTo(0));
        assert_eq!(app.tab, Tab::Sessions);
        app.reduce(Action::CycleTab(false));
        assert_eq!(app.tab, Tab::Intent);
    }

    #[test]
    fn opening_starts_on_the_first_file_with_unexplained_lines() {
        let state = ChangesState::new(vec![commit('1')], 0, Ok(set()));
        assert_eq!(state.file, 0);
        assert_eq!(state.focus, Focus::Files);
    }

    /// Wie `git diff`: Hunk-Kopf, alte und neue Nummer, `+`/`-` vor dem Text.
    #[test]
    fn the_unified_diff_reads_like_git_diff() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        let out = render(&mut app);
        assert!(out.contains("@@ -1,3 +1,4 @@"), "{out}");
        assert!(out.contains("1     1   fn order() {"), "{out}");
        assert!(out.contains("2       -     old();"), "{out}");
        assert!(out.contains("     2 +     sort_by_key();"), "{out}");
        assert!(out.contains("FILES · 1111111 · 1/1"), "{out}");
        assert!(out.contains("+2 −1"), "{out}");
        assert!(
            out.contains("◦1"),
            "the file list counts unexplained lines: {out}"
        );
    }

    /// Grün und Rot liegen als Hintergrund auf genau den `+`- und
    /// `-`-Zeilen; die unerklärte Zeile ist fett und invertiert markiert.
    #[test]
    fn added_lines_are_green_removed_red_and_unexplained_bold() {
        use ratatui::style::Modifier;
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        let mut terminal = Terminal::new(TestBackend::new(WIDE, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let find = |needle: &str| {
            (0..buffer.area.height)
                .find_map(|y| {
                    let line: String = (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect();
                    line.find(needle).map(|byte| {
                        let x = line[..byte].chars().count() as u16;
                        (x, y)
                    })
                })
                .unwrap_or_else(|| panic!("{needle} not drawn"))
        };
        let (x, y) = find("sort_by_key");
        assert_eq!(buffer[(x, y)].bg, super::super::changes::ADD_BG);
        let (x, y) = find("old();");
        assert_eq!(buffer[(x, y)].bg, super::super::changes::DEL_BG);
        let (x, y) = find("fn order");
        assert_ne!(buffer[(x, y)].bg, super::super::changes::ADD_BG);
        let (x, y) = find("// manuell");
        // Der Rand-Glyph der Zeile: drei Zeichen vor den Zeilennummern.
        let marker = (0..x)
            .rev()
            .find(|mx| buffer[(*mx, y)].symbol() == "◦")
            .unwrap();
        let cell = &buffer[(marker, y)];
        assert!(cell.modifier.contains(Modifier::BOLD | Modifier::REVERSED));
    }

    #[test]
    fn n_jumps_to_the_next_unexplained_line_across_files() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Unexplained(true));
        let state = app.changes.as_ref().unwrap();
        assert_eq!(state.focus, Focus::Diff);
        assert_eq!((state.file, state.row), (0, 4));
        app.reduce(Action::Unexplained(true));
        let state = app.changes.as_ref().unwrap();
        assert_eq!((state.file, state.row), (1, 1), "on into the next file");
        app.reduce(Action::Unexplained(true));
        let state = app.changes.as_ref().unwrap();
        assert_eq!((state.file, state.row), (0, 4), "round the circle");
        app.reduce(Action::Unexplained(false));
        let state = app.changes.as_ref().unwrap();
        assert_eq!((state.file, state.row), (1, 1), "and backwards");
    }

    #[test]
    fn brackets_jump_hunks_in_the_diff() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        assert_eq!(
            app.changes.as_ref().unwrap().row,
            1,
            "Enter lands on the first change"
        );
        app.reduce(Action::Bracket(false));
        assert_eq!(
            app.changes.as_ref().unwrap().row,
            0,
            "back to the hunk header"
        );
        app.reduce(Action::Bracket(true));
        assert_eq!(
            app.changes.as_ref().unwrap().row,
            0,
            "no further hunk: stays"
        );
        app.reduce(Action::Back);
        assert_eq!(app.changes.as_ref().unwrap().focus, Focus::Files);
        app.reduce(Action::Back);
        assert_eq!(app.tab, Tab::Sessions);
    }

    /// „Warum diese Zeile?": was die Session zu genau diesem Schritt
    /// festhielt — und für eine Zeile ohne Schreibvorgang der ehrliche Satz.
    #[test]
    fn the_why_column_explains_the_line_under_the_cursor() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        app.reduce(Action::Down);
        app.reduce(Action::Down);
        let out = render(&mut app);
        assert!(out.contains("WHY THIS LINE?"), "{out}");
        assert!(out.contains("REPORTED ONLY"), "{out}");
        assert!(out.contains("turn 1 · Edit (call 2)"), "{out}");
        assert!(out.contains("Ich lese und ändere."), "{out}");
        assert!(out.contains("Fix retry handling"), "{out}");
        assert!(out.contains("Review"), "{out}");

        app.reduce(Action::Down);
        let out = render(&mut app);
        assert!(out.contains("NOT OBSERVED"), "{out}");
        assert!(out.contains("No evidence of the sessions linked"), "{out}");

        app.reduce(Action::Why);
        assert!(
            !render(&mut app).contains("WHY THIS LINE?"),
            "w hides the column"
        );
    }

    /// Eine unerklärte Zeile sagt, warum — und nennt den Shell-Aufruf, der
    /// die Datei nennt.
    #[test]
    fn the_why_column_names_the_gap_of_an_unexplained_line() {
        use minds_reader::reconcile::Gap;
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        let shell = LineSource {
            session: sid('a'),
            turn: 0,
            call: 2,
        };
        let set_gap = |app: &mut App, gap| {
            if let Ok(set) = &mut app.changes.as_mut().unwrap().set {
                set.files[0].gap = Some(gap);
            }
        };
        set_gap(&mut app, Gap::Shell(shell));
        app.reduce(Action::Enter);
        for _ in 0..3 {
            app.reduce(Action::Down);
        }
        let out = render_at(&mut app, 260);
        assert!(out.contains("NOT OBSERVED"), "{out}");
        assert!(
            out.contains("No tool claim for this file. A shell"),
            "{out}"
        );
        assert!(out.contains("Shell    turn 1 · Bash (call 3)"), "{out}");
        assert!(out.contains("Agent said (turn 1, unverified)"), "{out}");
        assert!(out.contains("Session  b3-aaaaaaaaa"), "{out}");

        set_gap(&mut app, Gap::AfterAgent { later_shell: None });
        let out = render_at(&mut app, 260);
        assert!(
            out.contains("The last tool claim on this file (write or"),
            "{out}"
        );
        assert!(!out.contains("Shell    turn"), "{out}");

        set_gap(&mut app, Gap::Untouched { complete: true });
        let out = render_at(&mut app, 260);
        assert!(
            out.contains("No tool claim or shell mention of this"),
            "{out}"
        );
    }

    #[test]
    fn split_shows_old_and_new_side_by_side_on_a_wide_terminal() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        app.reduce(Action::Split);
        app.reduce(Action::Why);
        let out = render_at(&mut app, 260);
        let line = out
            .lines()
            .find(|l| l.contains("old();"))
            .unwrap_or_else(|| panic!("{out}"));
        assert!(
            line.contains("sort_by_key"),
            "removed and added side by side: {line}"
        );
        assert!(line.contains('│'), "{line}");
        assert!(out.contains("[split]"), "{out}");
        // Zu schmal: zurück zu Unified, ohne Fehler.
        let narrow = render_at(&mut app, 120);
        assert!(
            !narrow
                .lines()
                .any(|l| l.contains("old();") && l.contains("sort_by_key")),
            "{narrow}"
        );
    }

    #[test]
    fn split_pairs_put_removals_beside_additions() {
        let pairs = ChangesState::split_pairs(&set().files[0].rows);
        assert_eq!(
            pairs,
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), Some(3)),
                (None, Some(4)),
                (Some(5), Some(5)),
            ]
        );
    }

    #[test]
    fn the_footer_names_the_commit_and_the_keys_of_the_focus() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        let out = render(&mut app);
        assert!(out.contains("1111111 feat(sort): chronologisch"), "{out}");
        assert!(out.contains("Enter diff"), "{out}");
        app.reduce(Action::Enter);
        let out = render(&mut app);
        assert!(out.contains("n N unexplained"), "{out}");
    }

    /// Von Evidence → ARTIFACT mit Enter in den Diff des Commits.
    #[test]
    fn enter_on_artifact_opens_the_changes_of_that_commit() {
        use minds_reader::artifact::{ArtifactState, CommitArtifact};
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        let report = app.inspection.evidence_report(sid('a'));
        app.views.push(View::Evidence {
            id: sid('a'),
            report,
            uninterpreted: 0,
            artifacts: vec![CommitArtifact {
                commit: commit('1'),
                subject: None,
                inferred: false,
                claimants: 1,
                state: ArtifactState::Unavailable("test"),
            }],
            cursor: crate::app::ARTIFACT_SECTION,
        });
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Changes);
        let state = app.changes.as_ref().unwrap();
        assert_eq!(state.commit(), Some(commit('1')));
        // Der Evidence-Report bleibt im Sessions-Tab liegen.
        assert_eq!(app.views.len(), 1);
    }

    /// Neu laden hält Commit, Datei und Stelle — und liest den Commit nicht
    /// neu, solange sich seine Eingaben nicht ändern.
    #[test]
    fn reload_keeps_the_place_in_the_changes_tab() {
        let (_dir, repo) = repo();
        let mut app = App::new(filled().with_head(Some(commit('1'))), &repo, None);
        app.stamp = Some("1".into());
        app.changes = Some(ChangesState::new(vec![commit('1')], 0, Ok(set())));
        app.tab = Tab::Changes;
        app.reduce(Action::Down);
        app.reduce(Action::Enter);
        app.reload(
            Ok(filled().with_head(Some(commit('1')))),
            Some("2".into()),
            now(),
        );
        let state = app.changes.as_ref().unwrap();
        assert_eq!(state.current().map(|f| f.path.as_str()), Some("tests/t.rs"));
        assert_eq!(state.focus, Focus::Diff);
        assert!(
            state.set.is_ok(),
            "carried over, not re-read from the empty repo"
        );
    }

    /// Split ab 160 Spalten Terminal — auch mit eingeblendeter Begründung
    /// (die rückt dann unter den Diff). Darunter sagt der Kopf ehrlich, dass
    /// Unified gezeigt wird.
    #[test]
    fn split_appears_at_160_columns_with_the_why_column_on() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        app.reduce(Action::Split);
        assert!(app.changes.as_ref().unwrap().why);
        let out = render_at(&mut app, 160);
        assert!(
            out.lines()
                .any(|l| l.contains("old();") && l.contains("sort_by_key")),
            "{out}"
        );
        assert!(out.contains("WHY THIS LINE?"), "{out}");
        let out = render_at(&mut app, 159);
        assert!(out.contains("split needs ≥160 columns"), "{out}");
    }

    /// Im Split wandert der Cursor paarweise; er steht auf der neuen Seite,
    /// damit Rand und Begründung dieselbe Zeile meinen.
    #[test]
    fn split_moves_pair_by_pair_on_the_new_side() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        app.reduce(Action::Enter);
        app.reduce(Action::Split);
        render_at(&mut app, 200);
        assert_eq!(app.changes.as_ref().unwrap().row, 1);
        app.reduce(Action::Down);
        assert_eq!(
            app.changes.as_ref().unwrap().row,
            3,
            "pair (old();, sort_by_key)"
        );
        app.reduce(Action::Down);
        assert_eq!(app.changes.as_ref().unwrap().row, 4);
        app.reduce(Action::Up);
        assert_eq!(app.changes.as_ref().unwrap().row, 3);
        let out = render_at(&mut app, 200);
        assert!(
            out.contains("REPORTED ONLY"),
            "the why column means the new side: {out}"
        );
    }

    /// Ein Commit außerhalb der HEAD-Historie (über ARTIFACT geöffnet) bleibt
    /// nach dem Neuladen stehen.
    #[test]
    fn reload_keeps_a_commit_opened_from_outside_the_history() {
        let (_dir, repo) = repo();
        let mut app = App::new(filled().with_head(Some(commit('2'))), &repo, None);
        app.stamp = Some("1".into());
        app.changes = Some(ChangesState::new(
            vec![commit('2'), commit('9')],
            1,
            Ok(set()),
        ));
        app.tab = Tab::Changes;
        app.reload(
            Ok(filled().with_head(Some(commit('2')))),
            Some("2".into()),
            now(),
        );
        assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('9')));
    }

    /// Breite Zeichen zählen doppelt: Die neue Seite des Splits bleibt
    /// sichtbar, auch wenn die alte aus Vollbreit-Zeichen besteht.
    #[test]
    fn wide_characters_do_not_push_the_new_side_out_of_view() {
        let (_dir, repo) = repo();
        let mut wide = set();
        wide.files[0].rows[2].text = "ｗ".repeat(200);
        let mut app = App::new(filled(), &repo, None);
        app.changes = Some(ChangesState::new(vec![commit('1')], 0, Ok(wide)));
        app.tab = Tab::Changes;
        app.reduce(Action::Enter);
        app.reduce(Action::Split);
        let out = render_at(&mut app, 200);
        assert!(
            out.lines()
                .any(|l| l.contains('ｗ') && l.contains("sort_by_key")),
            "{out}"
        );
    }

    #[test]
    fn w_toggles_the_why_column_from_the_file_list_too() {
        let (_dir, repo) = repo();
        let mut app = app_with_changes(&repo);
        assert_eq!(app.changes.as_ref().unwrap().focus, Focus::Files);
        app.reduce(Action::Why);
        assert!(!app.changes.as_ref().unwrap().why);
    }
}

// --- Verify-Tab -------------------------------------------------------------

mod verify_tab {
    use std::cell::{Cell, RefCell};

    use minds_reader::assurance::{Assurance, IntentSignature, IntentState, SignerKind};
    use minds_reader::reconcile::{FileRecon, LineLevel, LineRecon, ReconClass, Reconciliation};

    use super::*;
    use crate::app::Tab;
    use crate::verify::{ClassCounts, Overall};
    use crate::{CommitVerify, SessionAssurance, VerifyVerdict};

    /// Eine Quelle, die das signaturabhängige Urteil vorgibt und zählt, wie
    /// oft es angefragt wurde.
    struct Signers {
        answer: RefCell<Option<CommitVerify>>,
        asked: Cell<usize>,
    }

    impl crate::Source for Signers {
        fn load(&self) -> minds_reader::Result<Inspection> {
            Ok(filled().with_head(Some(commit('1'))))
        }
        fn stamp(&self) -> Option<crate::Stamp> {
            Some("1".into())
        }
        fn verify(&self, _commit: CommitId) -> Result<CommitVerify, String> {
            self.asked.set(self.asked.get() + 1);
            self.answer
                .borrow()
                .clone()
                .ok_or_else(|| "store unreadable".to_string())
        }
    }

    fn witnessed(tampered: bool) -> CommitVerify {
        verdict(
            tampered,
            if tampered {
                VerifyVerdict::Tampered
            } else {
                VerifyVerdict::Verified
            },
        )
    }

    /// Wie [`witnessed`], mit dem Urteil, das `minds verify` fällt.
    fn verdict(tampered: bool, verdict: VerifyVerdict) -> CommitVerify {
        CommitVerify {
            verdict: Ok(verdict),
            sessions: vec![SessionAssurance {
                session: sid('a'),
                level: Assurance::A2Witnessed,
                reason: Some("not reproduced in CI".into()),
                intent: IntentState::Bound {
                    anchor_id: minds_core::ContentHash::from_bytes([7u8; 32]),
                    chained: true,
                    signature: IntentSignature::Valid(SignerKind::SoftwareKey),
                    snapshot_matches: true,
                    from_session_start: true,
                    changed_mid_session: false,
                },
                tampered,
            }],
            out_of_scope: Some(vec!["Cargo.lock".into()]),
            out_of_scope_paths: vec!["Cargo.lock".into()],
            scope_note: None,
        }
    }

    fn signers(answer: Option<CommitVerify>) -> Signers {
        Signers {
            answer: RefCell::new(answer),
            asked: Cell::new(0),
        }
    }

    fn app(repo: &Repo) -> App<'_> {
        let mut app = App::new(filled().with_head(Some(commit('1'))), repo, None);
        app.reduce(Action::TabTo(1));
        app
    }

    /// Sofort da: Urteil und Achsen aus dem Reader; die Signaturen werden
    /// noch geprüft — und das steht da, statt zu raten.
    #[test]
    fn the_verdict_comes_first_and_says_what_is_still_being_checked() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        assert_eq!(app.tab, Tab::Verify);
        let out = render(&mut app);
        // Ohne Signaturprüfung kein VERIFIED — der Reader allein sieht keine
        // ungültige Witness-Signatur.
        assert!(out.contains("… CHECKING"), "{out}");
        assert!(!out.contains("VERIFIED"), "{out}");
        assert!(out.contains("A1 observed (checking signatures…)"), "{out}");
        // Die Zeile nennt Fakten, kein eigenes Verdikt — das sagt die
        // Kopfzeile, von `verify` selbst.
        assert!(out.contains("· hash-valid"), "{out}");
        assert!(!out.contains("◈ sealed"), "{out}");
        // Vor der Prüfung neutral: hash-valid ja, Urteil noch nicht.
        assert!(
            out.contains("Integrity   · 1 seal(s) hash-valid · signatures checking…"),
            "{out}"
        );
        assert!(
            out.contains("Coverage    · signatures checking… · boundary agent-hooks/v1"),
            "{out}"
        );
        assert!(out.contains("Artifact    · not assessed"), "{out}");
        assert!(out.contains("Scope       … checking"), "{out}");
        assert!(out.contains("Not proven"), "{out}");
        assert!(out.contains("model identity"), "{out}");
    }

    /// Mit den Signern: Stufe, Intent und Scope aus der Quelle — einmal
    /// geholt, nicht je Frame.
    #[test]
    fn the_signed_parts_come_from_the_source_once() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let source = signers(Some(witnessed(false)));
        app.fill_verify(&source);
        app.fill_verify(&source);
        assert_eq!(source.asked.get(), 1);
        let out = render(&mut app);
        assert!(out.contains("✓ VERIFIED"), "{out}");
        assert!(out.contains("A2 witnessed"), "{out}");
        assert!(!out.contains("checking signatures"), "{out}");
        assert!(out.contains("intent signed (software key)"), "{out}");
        assert!(out.contains("chained"), "{out}");
        assert!(out.contains("⚠ 1 path(s) outside: Cargo.lock"), "{out}");
        assert!(out.contains("not reproduced in CI"), "{out}");
    }

    #[test]
    fn an_invalid_witness_signature_is_tampered() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.fill_verify(&signers(Some(witnessed(true))));
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::Tampered);
        let out = render(&mut app);
        assert!(out.contains("✗ TAMPERED"), "{out}");
    }

    #[test]
    fn a_failed_check_is_never_verified_and_says_why() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.fill_verify(&signers(None));
        let out = render(&mut app);
        assert!(out.contains("NOT CHECKED — run minds verify"), "{out}");
        assert!(!out.contains("✓ VERIFIED"), "{out}");
        assert!(
            out.contains("Signatures could not be checked: store unreadable"),
            "{out}"
        );
        assert!(out.contains("(signatures not checked)"), "{out}");
        assert!(
            out.contains("Scope       · not checked (store unreadable)"),
            "{out}"
        );
    }

    /// Eine Session ohne Seal: nichts zu verifizieren — wie `verify`.
    #[test]
    fn a_session_without_a_seal_is_not_verifiable() {
        let (_dir, repo) = repo();
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('b'), session("legacy", "2026-07-25T13:41:00Z"));
        let mut commits = BTreeMap::new();
        commits.insert(commit('1'), vec![sid('b')]);
        let inspection =
            Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t")
                .with_head(Some(commit('1')));
        let mut app = App::new(inspection, &repo, None);
        app.reduce(Action::TabTo(1));
        let mut legacy = verdict(false, VerifyVerdict::NotVerifiable);
        legacy.sessions[0].session = sid('b');
        app.fill_verify(&signers(Some(legacy)));
        let out = render(&mut app);
        assert!(out.contains("? NOT VERIFIABLE"), "{out}");
        assert!(out.contains("· no seal"), "{out}");
    }

    /// Ein verdeckter Tab startet kein ssh-keygen; sichtbar wird geprüft.
    #[test]
    fn a_hidden_tab_does_not_check() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.reduce(Action::TabTo(0));
        let source = signers(Some(witnessed(false)));
        app.fill_verify(&source);
        assert_eq!(source.asked.get(), 0);
        app.reduce(Action::TabTo(1));
        app.fill_verify(&source);
        assert_eq!(source.asked.get(), 1);
    }

    /// Enter auf Scope führt in den Diff des Commits.
    #[test]
    fn enter_on_scope_opens_the_diff() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.fill_verify(&signers(Some(witnessed(false))));
        app.reduce(Action::End);
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Changes);
        assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('1')));
    }

    #[test]
    fn class_counts_follow_lines_and_file_level_classes() {
        let line = |line, class| LineRecon {
            line,
            class,
            source: None,
        };
        let file = |level, class, changed| FileRecon {
            path: "a".into(),
            class,
            line_level: level,
            committed: minds_core::ContentHash::from_bytes([0u8; 32]),
            deleted: false,
            changed_lines: changed,
            removes: false,
            last_observed: None,
            gap: None,
        };
        let recon = Reconciliation {
            commit: commit('1'),
            base: None,
            files: vec![
                file(
                    LineLevel::Available(vec![
                        line(1, ReconClass::ReportedOnly),
                        line(2, ReconClass::Unexplained),
                        line(3, ReconClass::Explained),
                    ]),
                    ReconClass::ReportedOnly,
                    3,
                ),
                file(
                    LineLevel::Unavailable(minds_reader::reconcile::Reason::Binary),
                    ReconClass::ExplainedFsOnly,
                    1,
                ),
            ],
            explained_lines: 2,
            total_changed_lines: 4,
        };
        let counts = ClassCounts::of(&recon);
        assert_eq!(
            (
                counts.explained,
                counts.fs_only,
                counts.reported,
                counts.unexplained
            ),
            (1, 1, 1, 1)
        );
        assert_eq!((counts.backed(), counts.total()), (3, 4));
    }

    #[test]
    fn enter_jumps_to_the_session_the_diff_and_back() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        // Zeile 0: Session a → ihr Graph im Sessions-Tab.
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Sessions);
        assert!(matches!(app.top(), Some(View::Graph { id, .. }) if *id == sid('a')));
        // Artefakt → der Diff des Commits.
        app.reduce(Action::TabTo(1));
        app.reduce(Action::Down);
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Changes);
        assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('1')));
        // Esc im Verify-Tab: zurück zu den Sessions.
        app.reduce(Action::TabTo(1));
        app.reduce(Action::Back);
        assert_eq!(app.tab, Tab::Sessions);
    }

    /// Neu laden prüft die Signer immer neu: Eine ausgetauschte `seal.sig`
    /// ändert keine Session-Id — ein übernommenes Urteil könnte VERIFIED
    /// zeigen, wo `verify` TAMPERED sagt.
    #[test]
    fn reload_always_checks_the_signers_again() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.stamp = Some("1".into());
        app.fill_verify(&signers(Some(witnessed(false))));
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::Verified);
        let tampered = signers(Some(witnessed(true)));
        app.reload(
            Ok(filled().with_head(Some(commit('1')))),
            Some("2".into()),
            now(),
        );
        assert!(app.verify.as_ref().unwrap().pending);
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::Checking);
        app.fill_verify(&tampered);
        assert_eq!(tampered.asked.get(), 1);
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::Tampered);
    }

    /// Das Urteil ist das von `minds verify`: Sieht `verify` eine Lücke, die
    /// der Reader nicht kennt (ein Seal nur im Namensraum), gilt INCOMPLETE.
    #[test]
    fn the_verdict_is_the_one_of_minds_verify() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.fill_verify(&signers(Some(verdict(false, VerifyVerdict::Incomplete))));
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::Incomplete);
        let out = render(&mut app);
        assert!(out.contains("! VERIFIED, INCOMPLETE"), "{out}");
        assert!(out.contains("Coverage    ! incomplete"), "{out}");
    }

    /// Ohne geprüftes Urteil nie VERIFIED und nie INCOMPLETE — beides
    /// behauptet intakte Integrität.
    #[test]
    fn without_a_checked_verdict_neither_verified_nor_incomplete() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let mut failed = witnessed(false);
        failed.verdict = Err("minds verify failed".into());
        app.fill_verify(&signers(Some(failed)));
        let out = render(&mut app);
        assert!(out.contains("NOT CHECKED"), "{out}");
        assert!(!out.contains("VERIFIED"), "{out}");
        assert!(out.contains("Integrity   · 1 seal(s) hash-valid"), "{out}");
        assert!(!out.contains("✓ intact"), "{out}");
        assert!(out.contains("minds verify failed"), "{out}");
    }

    /// Prüfte die CLI andere Sessions als gezeigt, gilt das Urteil nicht.
    #[test]
    fn a_different_session_set_is_not_checked() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let mut other = witnessed(false);
        other.sessions[0].session = sid('e');
        app.fill_verify(&signers(Some(other)));
        assert_eq!(app.verify.as_ref().unwrap().overall(), Overall::NotChecked);
        let out = render(&mut app);
        assert!(out.contains("checked sessions differ"), "{out}");
    }

    /// Die Drossel: Vom Nutzer verlangt sofort, nach einem Neuladen erst
    /// wieder nach `every` — und ohne ausstehende Prüfung nie.
    #[test]
    fn checks_after_a_reload_are_throttled_but_user_requests_are_not() {
        use std::time::Duration;
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let every = Duration::from_secs(5);
        let state = app.verify.as_ref().unwrap();
        assert!(state.due(Duration::ZERO, every), "opened: at once");
        app.fill_verify(&signers(Some(witnessed(false))));
        assert!(
            !app.verify.as_ref().unwrap().due(every, every),
            "nothing pending"
        );
        app.stamp = Some("1".into());
        app.reload(
            Ok(filled().with_head(Some(commit('1')))),
            Some("2".into()),
            now(),
        );
        let state = app.verify.as_ref().unwrap();
        assert!(
            !state.due(Duration::from_secs(1), every),
            "after a reload: throttled"
        );
        assert!(state.due(every, every));
        // `r` (voll) prüft sofort.
        app.full_next = true;
        app.reload(
            Ok(filled().with_head(Some(commit('1')))),
            Some("3".into()),
            now(),
        );
        assert!(app.verify.as_ref().unwrap().due(Duration::ZERO, every));
    }

    /// TAMPERED von `verify` selbst: kein ✓-Scope und keine Stufe aus einer
    /// älteren Momentaufnahme daneben — `verify` zeigt dann beides nicht.
    #[test]
    fn a_tampered_verdict_shows_neither_scope_nor_level() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let mut cli = verdict(false, VerifyVerdict::Tampered);
        cli.out_of_scope = Some(Vec::new());
        cli.out_of_scope_paths = Vec::new();
        app.fill_verify(&signers(Some(cli)));
        let out = render(&mut app);
        assert!(out.contains("✗ TAMPERED"), "{out}");
        assert!(
            out.contains("Scope       · not assessed (integrity violated)"),
            "{out}"
        );
        assert!(!out.contains("✓ every path"), "{out}");
        assert!(!out.contains("A2 witnessed"), "{out}");
    }

    /// Kein ✓ „intact" neben NOT VERIFIABLE.
    #[test]
    fn not_verifiable_shows_no_intact_check() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.fill_verify(&signers(Some(verdict(false, VerifyVerdict::NotVerifiable))));
        let out = render(&mut app);
        assert!(out.contains("? NOT VERIFIABLE"), "{out}");
        assert!(out.contains("Integrity   · 1 seal(s) hash-valid"), "{out}");
        assert!(!out.contains("✓ intact"), "{out}");
    }

    /// Lücken und Events kommen aus dem Store: sättigend summiert, kein
    /// Überlauf-Panic.
    #[test]
    fn huge_gap_counts_saturate() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let state = app.verify.as_mut().unwrap();
        let mut row = state.sessions[0].clone();
        row.report.as_mut().unwrap().state.gaps = u64::MAX;
        state.sessions = vec![row.clone(), row];
        let out = render(&mut app);
        assert!(out.contains(&format!("{} gap(s)", u64::MAX)), "{out}");
    }

    /// Der Balken rundet ab: 999 von 1000 ist nicht voll.
    #[test]
    fn the_artifact_bar_is_never_full_with_an_unexplained_line() {
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.verify.as_mut().unwrap().artifact = Ok(ClassCounts {
            explained: 999,
            unexplained: 1,
            ..ClassCounts::default()
        });
        let out = render(&mut app);
        assert!(out.contains("99 %"), "{out}");
        assert!(!out.contains(&"█".repeat(24)), "{out}");
    }

    /// Zurück im Tab mit ausstehender Prüfung: sofort prüfen, nicht drosseln.
    #[test]
    fn returning_to_the_tab_checks_a_pending_reload_at_once() {
        use std::time::Duration;
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        let every = Duration::from_secs(5);
        app.fill_verify(&signers(Some(witnessed(false))));
        app.reduce(Action::TabTo(0));
        app.stamp = Some("1".into());
        app.reload(
            Ok(filled().with_head(Some(commit('1')))),
            Some("2".into()),
            now(),
        );
        assert!(!app.verify.as_ref().unwrap().due(Duration::ZERO, every));
        app.reduce(Action::TabTo(1));
        assert!(app.verify.as_ref().unwrap().due(Duration::ZERO, every));
    }

    /// Warum nicht 100 %: die unerklärten Zeilen nach ihrem Grund.
    #[test]
    fn the_artifact_says_why_lines_are_unexplained() {
        use minds_reader::reconcile::GapCounts;
        let (_dir, repo) = repo();
        let mut app = app(&repo);
        app.verify.as_mut().unwrap().artifact = Ok(ClassCounts {
            explained: 6,
            unexplained: 4,
            gaps: GapCounts {
                after_agent: 1,
                untouched: 3,
                ..GapCounts::default()
            },
            ..ClassCounts::default()
        });
        let out = render_at(&mut app, 200);
        assert!(
            out.contains(
                "◦ 1 whose last agent claim is another version or a deletion · 3 with no tool claim or shell mention found"
            ),
            "{out}"
        );
    }
}

mod intent_tab {
    use std::cell::{Cell, RefCell};

    use minds_core::ContentHash;
    use minds_core::intent_anchor::IntentEvent;
    use minds_reader::assurance::{IntentSignature, SignerKind};
    use minds_reader::model::WhyStep;

    use super::*;
    use crate::app::{Tab, View};
    use crate::{IntentDetail, IntentInfo, IntentList};

    fn anchor(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    /// Session a nennt Anker `a_names` per Witness-Event, Session b Anker 3
    /// per lokaler Datei — den der Store nicht hat.
    fn anchored_to(a_names: u8) -> Inspection {
        let mut a = session("Fix retry handling", "2026-07-25T14:10:00Z");
        a.intent_events.push(IntentEvent {
            seq: 1,
            anchor_id: anchor(a_names),
            opens_session: true,
        });
        let mut b = session("Add exponential backoff", "2026-07-25T13:41:00Z");
        b.intent_anchor = Some(anchor(3));
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), a);
        sessions.insert(sid('b'), b);
        let mut commits = BTreeMap::new();
        commits.insert(commit('1'), vec![sid('a')]);
        Inspection::from_index(Index::from_parts(sessions, commits), Vec::new(), "t")
    }

    fn anchored() -> Inspection {
        anchored_to(1)
    }

    fn proven(byte: u8) -> IntentInfo {
        IntentInfo {
            id: anchor(byte),
            active: byte == 1,
            detail: Ok(IntentDetail {
                source: "file:docs/req.md@3f9c1e2a".into(),
                content: anchor(9),
                scope: vec!["src/sort/**".into(), "tests/**".into()],
                version: Some("the version in HEAD".into()),
                proof: Ok(()),
                signature: IntentSignature::Valid(SignerKind::SoftwareKey),
                snapshot_len: 20,
                snapshot: Some(vec!["AC1: sort by date".into()]),
                snapshot_clipped: true,
            }),
        }
    }

    fn unproven(byte: u8) -> IntentInfo {
        IntentInfo {
            id: anchor(byte),
            active: false,
            detail: Ok(IntentDetail {
                source: "prompt".into(),
                content: anchor(8),
                scope: Vec::new(),
                version: None,
                proof: Err("intent snapshot does not match its anchor".into()),
                signature: IntentSignature::Unsigned,
                snapshot_len: 11,
                // Eine Quelle, die die Regel bräche: Der View zeigt ihn trotzdem nicht.
                snapshot: Some(vec!["PLANTED_SECRET".into()]),
                snapshot_clipped: false,
            }),
        }
    }

    /// Wie die echte Quelle: die Anker des Stores, genannte, die fehlen,
    /// mit Grund.
    struct Anchors {
        asked: Cell<usize>,
        store: RefCell<Vec<IntentInfo>>,
        total: usize,
    }

    impl crate::Source for Anchors {
        fn load(&self) -> minds_reader::Result<Inspection> {
            Ok(anchored())
        }
        fn stamp(&self) -> Option<crate::Stamp> {
            None
        }
        fn intents(&self, named: &[ContentHash]) -> Result<IntentList, String> {
            self.asked.set(self.asked.get() + 1);
            let mut entries = self.store.borrow().clone();
            for id in named {
                if !entries.iter().any(|i| &i.id == id) {
                    entries.push(IntentInfo {
                        id: id.clone(),
                        active: false,
                        detail: Err("not in this store".into()),
                    });
                }
            }
            Ok(IntentList {
                total: self.total.max(entries.len()),
                entries,
                skipped: 0,
            })
        }
    }

    fn source() -> Anchors {
        Anchors {
            asked: Cell::new(0),
            store: RefCell::new(vec![proven(1), unproven(2)]),
            total: 0,
        }
    }

    fn opened<'r>(repo: &'r Repo, source: &Anchors) -> App<'r> {
        let mut app = App::new(anchored(), repo, None);
        app.reduce(Action::TabTo(3));
        app.fill_intents(source);
        app
    }

    /// Liste und Detail: geprüfte Signatur, Beleg, Bereich, Snapshot und
    /// wer sich an den Anker gebunden nennt — als Record.
    #[test]
    fn the_tab_shows_the_anchor_and_what_is_checked() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = opened(&repo, &source);
        assert_eq!(app.tab, Tab::Intent);
        assert_eq!(source.asked.get(), 1);
        let out = render(&mut app);
        assert!(out.contains("F4 Intent"), "{out}");
        assert!(out.contains("✓ b3-01010101010"), "{out}");
        assert!(out.contains("◉1"), "{out}");
        assert!(out.contains("(local file)"), "{out}");
        assert!(out.contains("valid (software key)"), "{out}");
        assert!(out.contains("src/sort/**, tests/**"), "{out}");
        assert!(out.contains("the version in HEAD"), "{out}");
        assert!(out.contains("proof     ok"), "{out}");
        assert!(out.contains("Sessions naming this anchor (1)"), "{out}");
        assert!(out.contains("(witness event)"), "{out}");
        assert!(out.contains("AC1: sort by date"), "{out}");
        assert!(out.contains("… cut here — minds intent show"), "{out}");
        assert!(!out.contains("anchors read"), "nothing cut: {out}");
        // Einmal geholt, nicht je Frame.
        app.fill_intents(&source);
        assert_eq!(source.asked.get(), 1);
    }

    /// Ein unbelegter Anker: kein Snapshot — auch wenn die Quelle einen
    /// mitgäbe —, der Grund steht da.
    #[test]
    fn an_unproven_anchor_hides_its_snapshot() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = opened(&repo, &source);
        app.reduce(Action::Down);
        let out = render(&mut app);
        assert!(
            out.contains("NOT PROVEN — intent snapshot does not match its anchor"),
            "{out}"
        );
        assert!(
            out.contains("Snapshot (11 bytes) not shown — the anchor is not proven"),
            "{out}"
        );
        assert!(!out.contains("PLANTED_SECRET"), "{out}");
        assert!(out.contains("scope     none declared"), "{out}");
        assert!(out.contains("signature none"), "{out}");
        assert!(out.contains("not the local file binding (A1)"), "{out}");
    }

    /// Ein Anker, den eine Session nennt, der Store aber nicht hat: gezeigt,
    /// mit dem Grund der Quelle.
    #[test]
    fn an_anchor_missing_from_the_store_is_listed() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = opened(&repo, &source);
        app.reduce(Action::End);
        let out = render(&mut app);
        assert!(out.contains("not in this store"), "{out}");
        assert!(
            out.contains("(local file)  Add exponential backoff"),
            "{out}"
        );
    }

    /// Gekappt: Der Tab sagt, wie viele er las.
    #[test]
    fn a_cut_list_says_so() {
        let (_dir, repo) = repo();
        let mut source = source();
        source.total = 700;
        let mut app = opened(&repo, &source);
        let out = render(&mut app);
        assert!(out.contains("3 of 700 anchors read"), "{out}");
    }

    /// Nach einem Neuladen gilt nichts mehr als geprüft, bis neu geholt ist.
    #[test]
    fn after_a_reload_nothing_is_shown_as_checked_until_rechecked() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = opened(&repo, &source);
        app.stamp = Some(crate::Stamp {
            head: "h".into(),
            refs: "1".into(),
        });
        app.reload(
            Ok(anchored()),
            Some(crate::Stamp {
                head: "h".into(),
                refs: "2".into(),
            }),
            now(),
        );
        let out = render(&mut app);
        assert!(!out.contains("valid (software key)"), "{out}");
        assert!(!out.contains("AC1: sort by date"), "{out}");
        assert!(!out.contains("the version in HEAD"), "{out}");
        assert!(!out.contains("✓ b3-"), "{out}");
        assert!(out.contains("rechecking…"), "{out}");
        // Von selbst gedrosselt, dann wieder geprüft.
        let state = app.intent.as_ref().unwrap();
        let every = std::time::Duration::from_secs(5);
        assert!(!state.due(std::time::Duration::from_secs(1), every));
        assert!(state.due(every, every));
        app.fill_intents(&source);
        assert!(render(&mut app).contains("valid (software key)"));
    }

    /// Ein verdeckter Tab fragt die Quelle nicht.
    #[test]
    fn a_hidden_tab_does_not_ask() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = App::new(anchored(), &repo, None);
        app.fill_intents(&source);
        assert_eq!(source.asked.get(), 0);
    }

    /// Enter auf einem Anker: der Graph einer Session, die ihn nennt.
    #[test]
    fn enter_opens_the_graph_of_a_session_naming_the_anchor() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = opened(&repo, &source);
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Sessions);
        assert!(matches!(app.top(), Some(View::Graph { id, .. }) if *id == sid('a')));
    }

    /// In der Why-Kette ist der Anker ein Glied: Enter öffnet ihn im
    /// Intent-Tab, auf genau diesem Anker — hier dem zweiten der Liste.
    #[test]
    fn the_why_chain_links_to_the_anchor() {
        let (_dir, repo) = repo();
        let source = source();
        let mut app = App::new(anchored_to(2), &repo, Some("Fix retry".into()));
        app.reduce(Action::Why);
        let out = render(&mut app);
        assert!(out.contains("Anchor b3-02020202020"), "{out}");
        assert!(out.contains("(witness event, as recorded)"), "{out}");
        let at_intent = |app: &App| {
            matches!(
                app.top(),
                Some(View::Why { chain, cursor, .. })
                    if matches!(chain.steps.get(*cursor), Some(WhyStep::Intent { .. }))
            )
        };
        for _ in 0..10 {
            if at_intent(&app) {
                break;
            }
            app.reduce(Action::Down);
        }
        assert!(at_intent(&app), "the cursor reaches the INTENT step");
        app.reduce(Action::Enter);
        assert_eq!(app.tab, Tab::Intent);
        app.fill_intents(&source);
        let state = app.intent.as_ref().unwrap();
        assert_eq!(state.selected().map(|i| i.id.clone()), Some(anchor(2)));
        // Der Stapel bleibt: zurück im Sessions-Tab steht die Kette noch.
        app.reduce(Action::TabTo(0));
        assert!(matches!(app.top(), Some(View::Why { .. })));
        // Ein zweites Öffnen bei bestehendem Tab wählt sofort.
        app.open_intent(Some(anchor(1)));
        let state = app.intent.as_ref().unwrap();
        assert_eq!(state.selected().map(|i| i.id.clone()), Some(anchor(1)));
    }

    /// Viele Anker: Das Fenster folgt dem Cursor, ▸ bleibt sichtbar.
    #[test]
    fn the_list_scrolls_with_the_cursor() {
        let (_dir, repo) = repo();
        let source = source();
        source.store.replace((10..90).map(proven).collect());
        let mut app = opened(&repo, &source);
        app.reduce(Action::End);
        let out = render(&mut app);
        // Das Fenster folgt: der letzte Anker zu sehen, der erste nicht.
        assert!(out.contains("b3-5959595959"), "{out}");
        assert!(!out.contains("b3-0a0a0a0a0a"), "{out}");
    }

    /// Lange, umbrochene Snapshot-Zeilen: PgDn erreicht das Ende — die
    /// Grenze zählt gerenderte Zeilen, nicht logische.
    #[test]
    fn the_detail_scrolls_to_the_end_of_wrapped_lines() {
        let (_dir, repo) = repo();
        let source = source();
        let mut long = proven(1);
        if let Ok(detail) = &mut long.detail {
            let mut lines: Vec<String> = (0..30)
                .map(|i| format!("{i:02} {}", "word ".repeat(60)))
                .collect();
            lines.push("THE LAST LINE".into());
            detail.snapshot = Some(lines);
        }
        source.store.replace(vec![long]);
        let mut app = opened(&repo, &source);
        render_at(&mut app, 120);
        for _ in 0..200 {
            app.reduce(Action::PageDown);
        }
        let out = render_at(&mut app, 120);
        assert!(out.contains("… cut here"), "{out}");
        // Eine Seite zurück bewegt sofort.
        app.reduce(Action::PageUp);
        let back = render_at(&mut app, 120);
        assert_ne!(out, back);
    }
}
