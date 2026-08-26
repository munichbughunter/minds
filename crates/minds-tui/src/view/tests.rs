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
                capture: None,
                name: "Read".into(),
                arguments: "{}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Read,
                    path: Some("src/http/retry.rs".into()),
                    content: None,
                }),
            },
            ToolCall {
                capture: None,
                name: "Edit".into(),
                arguments: "{}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Write,
                    path: Some("src/http/retry.rs".into()),
                    content: None,
                }),
            },
            ToolCall {
                capture: None,
                name: "Bash".into(),
                arguments: "{\"command\":\"cargo test\"}".into(),
                effect: Some(Effect {
                    kind: EffectKind::Exec,
                    path: None,
                    content: None,
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

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(124, 30)).unwrap();
    terminal.draw(|frame| super::draw(frame, app)).unwrap();
    terminal.backend().to_string()
}

#[test]
fn the_empty_state_points_to_enable() {
    let (_dir, repo) = repo();
    let mut app = App::new(Inspection::default(), &repo, None);
    let out = render(&mut app);
    assert!(out.contains("Noch keine Sessions erfasst."), "{out}");
    assert!(out.contains("minds enable"), "{out}");
    assert!(out.contains("0 Sessions"), "{out}");
}

#[test]
fn the_list_shows_newest_first_with_evidence_verdict_and_a_degraded_row() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render(&mut app);
    assert!(out.contains("MINDS payment-service"), "{out}");
    assert!(out.contains("2 Sessions · 1 Changes"), "{out}");
    assert!(out.contains("1 degradiert"), "{out}");
    let fix = out.find("Fix retry handling").unwrap();
    let backoff = out.find("Add exponential backoff").unwrap();
    let forgotten = out
        .find("vergessen: DSGVO")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(fix < backoff && backoff < forgotten, "{out}");
    // Die Verdikt-Spalte (ADR-0011): Session a ist versiegelt, b nicht.
    assert!(out.contains("◈ versiegelt"), "{out}");
    // Session b hat keine Seals: explizit LEGACY, kein leeres Nichts.
    assert!(out.contains("· legacy"), "{out}");
    // Der Kanten-Beleg als Glyph mit Status-Modifikator — beobachtet heisst
    // nicht geprueft.
    assert!(out.contains("● ?"), "{out}");
    assert!(out.contains("↻ needs work"), "{out}");
    assert!(out.contains("⌦ vergessen"), "{out}");
    assert!(out.contains("25.07. 14:10Z"), "{out}");
    assert!(out.contains("Kontext-Abdeckung"), "{out}");
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
    assert!(out.contains("1/3 Treffer"), "{out}");
    assert!(out.contains("Add exponential backoff"), "{out}");
    assert!(!out.contains("Fix retry handling"), "{out}");
    app.reduce(Action::SearchCommit);
    let out = render(&mut app);
    assert!(out.contains("[backoff]"), "{out}");
    app.reduce(Action::Back);
    assert!(app.query.is_empty());
    assert_eq!(app.visible.len(), 3);
    // Kein Treffer ist ein Zustand, kein Fehler.
    app.reduce(Action::SearchStart);
    for c in "nirgends".chars() {
        app.reduce(Action::SearchInput(c));
    }
    let out = render(&mut app);
    assert!(out.contains("Kein Treffer"), "{out}");
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
    assert!(out.contains("ZEITLEISTE"), "{out}");
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
    // Session b ist nicht versiegelt — das Glied traegt seit ADR-0011 eine
    // ehrliche Luecke (UnsealedRange), zusaetzlich zur fehlenden Bewertung.
    assert!(out.contains("⚠ SESSION"), "{out}");
    assert!(out.contains("✓ AGENT"), "{out}");
    assert!(out.contains("✓ INTENT"), "{out}");
    assert!(out.contains("Add exponential backoff"), "{out}");
    assert!(out.contains("✓ EVIDENCE"), "{out}");
    assert!(out.contains("keine Kante"), "{out}");
    assert!(out.contains("offen"), "{out}");
    // Lücken sind First-Class: je Glied ✓/⚠, unten der Block mit Begründung.
    assert!(out.contains("⚠ REVIEW"), "{out}");
    assert!(out.contains(" 2 LÜCKEN "), "{out}");
    assert!(out.contains("nicht versiegelt"), "{out}");
    assert!(
        out.contains("Keine Bewertung — niemand hat diese Änderung entschieden."),
        "{out}"
    );
    assert!(out.contains("⚠ 2 Lücken in der Kette"), "{out}");
}

#[test]
fn a_fully_backed_chain_says_so_and_focus_explains_the_evidence_without_enter() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why); // „Fix retry handling" — Trailer, Change-Id, Review
    let out = render(&mut app);
    assert!(out.contains(" KEINE LÜCKE "), "{out}");
    assert!(out.contains("✓ EVIDENCE"), "{out}");
    assert!(out.contains("✓ keine Lücke"), "{out}");
    assert!(!out.contains("WHY IS THIS LINKED?"), "{out}");
    // Cursor auf das Evidence-Glied: Session, Agent, Intent, Evidence.
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    let out = render(&mut app);
    assert!(out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(out.contains("expliziter Herkunftsnachweis"), "{out}");
    assert!(out.contains("trägt den Trailer Minds-Session-Id"), "{out}");
}

#[test]
fn the_list_footer_says_what_the_focused_evidence_means() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render(&mut app);
    assert!(
        out.contains("Beobachtet: Der Commit trägt den Trailer"),
        "{out}"
    );
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(
        out.contains("Unverknüpft: Diese Session hängt an keinem Commit"),
        "{out}"
    );
    app.reduce(Action::End);
    let out = render(&mut app);
    assert!(out.contains("Degradiert:"), "{out}");
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
    assert!(out.contains("Beleg      ● ? observed [ungeprüft]"), "{out}");
    assert!(out.contains("expliziter Herkunftsnachweis"), "{out}");
}

#[test]
fn the_inspector_explains_an_observed_edge() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    // Zum Evidence-Glied: Session, Agent, Intent, Evidence.
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    app.reduce(Action::Enter);
    let out = render(&mut app);
    assert!(out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(out.contains("● ? observed [ungeprüft]"), "{out}");
    assert!(out.contains("trägt den Trailer Minds-Session-Id"), "{out}");
    // Der Satz kann am Zeilenumbruch brechen — kurzer, stabiler Anker.
    assert!(out.contains("Status: nie"), "{out}");
    // Esc schließt den Inspector; weg vom Evidence-Glied bleibt er zu.
    app.reduce(Action::Back);
    app.reduce(Action::Up);
    let out = render(&mut app);
    assert!(!out.contains("WHY IS THIS LINKED?"), "{out}");
    assert!(matches!(app.top(), Some(View::Why { .. })));
}

#[test]
fn an_inferred_edge_never_looks_like_an_observed_one() {
    // Glyph **und** Wort unterscheiden sich — nicht nur die Farbe, die in
    // einem monochromen Terminal verloren geht.
    let (glyph, word, _) =
        crate::theme::evidence(Some(EvidenceMark::of(EvidenceSource::Heuristic)));
    assert_eq!(glyph, "○ ?");
    assert!(word.contains("vermutet"));
    let (glyph, word, _) = crate::theme::evidence(Some(EvidenceMark::of(EvidenceSource::Observed)));
    assert_eq!(glyph, "● ?");
    assert_eq!(word, "observed [ungeprüft]");

    // Und ein nachgerechneter Beleg unterscheidet sich vom ungeprüften —
    // wieder in Glyph UND Wort.
    let verified = EvidenceMark {
        source: EvidenceSource::Observed,
        status: minds_core::EvidenceStatus::Verified,
    };
    let (glyph, word, _) = crate::theme::evidence(Some(verified));
    assert_eq!(glyph, "● ✓");
    assert!(word.contains("nachgerechnet"));
}

#[test]
fn the_help_overlays_and_closes() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Help);
    let out = render(&mut app);
    assert!(out.contains(" Hilfe "), "{out}");
    assert!(out.contains("inferred [vermutet]"), "{out}");
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
    assert!(out.contains("Blame kennt die Zeile nicht"), "{out}");
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
    assert_eq!(first[6], "observed [ungeprüft]");
    assert_eq!(first[7], "versiegelt");
    assert_eq!(first[8], "needs work");
    assert_eq!(first[10], "Fix retry handling");
    assert!(lines[2].contains("vergessen: DSGVO"));

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
        text.ends_with("niemand hat diese Änderung entschieden.\n"),
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
        capture: Some(minds_core::Capture {
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
    assert!(out.contains("BEOBACHTET"), "{out}");
}
