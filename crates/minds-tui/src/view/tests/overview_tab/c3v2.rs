//! C3 · Overview v2 — der Einstieg beantwortet drei Fragen: Welcher Commit
//! stammt von einem Agenten, was war der Auftrag, wie weit ist das belegt.
//! Jede Zahl ist konsistent: Was oben steht, lässt sich unten nachzählen.

use super::*;

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

// ---------------------------------------------------------------------
// C3.1 · Eine Quelle für alle Zahlen
// ---------------------------------------------------------------------

/// Die Titelzahl „with an agent session" ist die Zahl der Zeilen mit
/// Agent-Badge; die Zahl steht genau einmal — im Boxtitel —, ohne Prozent
/// und ohne „Changes".
#[test]
fn c31_the_title_counts_what_the_list_shows() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = lines_at(&mut app, w, h);
        let out = lines.join("\n");
        let title = &lines[2];
        assert!(title.contains("OVERVIEW · 3 commits"), "{w}: {out}");
        let state = app.overview.as_ref().unwrap();
        let counts = state.data.as_ref().unwrap().counts();
        assert!(
            title.contains(&format!(
                "{} with an agent session",
                counts.with_agent_session
            )),
            "{w}: {out}"
        );
        // Nur die Liste links zählt — das Detail rechts nennt die Sessions.
        let badges = lines
            .iter()
            .filter(|l| l.split("││").next().unwrap_or(l).contains("◉CC"))
            .count();
        // Die Session ohne Commit trägt auch ein Agent-Badge.
        let wip = state.data.as_ref().unwrap().wip.len();
        assert_eq!(badges, counts.with_agent_session + wip, "{w}: {out}");
        // Kopf und Titel: keine Prozente, keine Change-Ids, keine Doppelung.
        // (Zeile 1 trägt den Tab „Changes" — gemeint ist die Statistik.)
        for line in &lines[..3] {
            assert!(!line.contains("% of commits"), "{w}: {out}");
            assert!(!line.contains("Changes ·"), "{w}: {out}");
        }
        for line in &lines[1..3] {
            assert!(!line.contains("Changes"), "{w}: {out}");
        }
        assert!(!lines[1].contains("Sessions"), "{w}: {out}");
        assert_eq!(
            out.matches("with an agent session").count(),
            1,
            "{w}: {out}"
        );
    }
}

/// Kopfzeile 2 ist nur der Breadcrumb — die Statistik ist aus dem Kopf
/// verschwunden, in jedem Tab.
#[test]
fn c31_the_second_header_line_is_only_the_breadcrumb() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let lines = lines_at(&mut app, 300, 30);
    assert_eq!(lines[1].trim_end(), app.breadcrumb().join(" › "));
    // Die Zahl der Sessions bleibt erreichbar — im Rahmen der Liste.
    assert!(
        lines.iter().any(|l| l.contains(" SESSIONS · 2 captured")),
        "{}",
        lines.join("\n")
    );
}

/// Gekappt: Der Titel sagt bei der Zahl, dass nur das Gelesene zählt.
#[test]
fn c31_a_capped_title_says_what_was_read() {
    use minds_reader::overview::OverviewCounts;
    let counts = OverviewCounts {
        commits: 500,
        with_agent_session: 7,
        without_agent_session: 493,
        sessions_without_commit: 0,
        limited: true,
    };
    let text = crate::view::overview::counts_title(&counts, false);
    assert_eq!(text, "500 commits read (limit) · 7 with an agent session");
    let text = crate::view::overview::counts_title(&counts, true);
    assert_eq!(
        text,
        "500 commits read (read budget reached) · 7 with an agent session"
    );
    let open = OverviewCounts {
        limited: false,
        commits: 4,
        with_agent_session: 1,
        without_agent_session: 3,
        ..counts
    };
    assert_eq!(
        crate::view::overview::counts_title(&open, false),
        "4 commits · 1 with an agent session"
    );
    assert!(!crate::view::overview::counts_title(&open, false).contains('%'));
}

// ---------------------------------------------------------------------
// C3.2 · Die Liste: Betreff vorn, Auftrag sichtbar, Status leise
// ---------------------------------------------------------------------

/// Das Raster der Liste — die Zeilen links der Grenze zum Detail.
fn list_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.split("││").next().unwrap_or(l).to_string())
        .collect()
}

/// Spalten: Lanes · Badge · Betreff · Status · Refs · Zeit. Agent-Commits
/// tragen eine zweite, gedimmte Zeile mit dem Auftrag; menschliche bleiben
/// einzeilig. Der Graph läuft mit `│` an der zweiten Zeile vorbei.
#[test]
fn c32_subject_status_refs_time_and_the_request_below() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let lines = list_lines(&lines_at(&mut app, 160, 40));
    let out = lines.join("\n");
    let at = lines
        .iter()
        .position(|l| l.contains("feat(sort): agent work"))
        .expect("agent row");
    let row = &lines[at];
    let subject = row.find("feat(sort): agent work").unwrap();
    let status = row.find("· unsealed").expect("status");
    // Frisch angelegt: „now" — sonst kurz „3h", „1d" (C3.2).
    let time = row.rfind(" now").expect("relative time");
    assert!(subject < status && status < time, "{out}");
    // Der Auftrag steht nicht mehr in der Zeile, sondern darunter.
    assert!(!row.contains("Fix retry handling"), "{out}");
    assert!(!out.contains("• Fix retry handling"), "{out}");
    let below = &lines[at + 1];
    assert!(below.contains("Fix retry handling"), "{out}");
    assert!(
        below.trim_start_matches(['│', ' ']).starts_with("│") || below.contains("│ "),
        "{out}"
    );
    // HEAD-Zeile: Refs als leise Textspalte zwischen Status und Zeit.
    let head = lines
        .iter()
        .find(|l| l.contains("docs: two sessions"))
        .unwrap();
    let refs = head.find("@main ▲ HEAD").expect("refs column");
    assert!(head.find("◉2").unwrap() < refs, "{out}");
    assert!(refs < head.rfind(" now").unwrap(), "{out}");
    // Der menschliche Commit bleibt einzeilig.
    let human = lines
        .iter()
        .position(|l| l.contains("init: empty"))
        .unwrap();
    assert!(
        lines[human + 1]
            .trim_matches(['│', ' ', '└', '─', '┘'])
            .is_empty(),
        "{out}"
    );
    // Keine farbigen Pillen mehr: der Auftrag nicht hinter einem `•`, das
    // Requirement-Zeichen ohne Pillen-Wort.
    assert!(out.contains("⚑"), "{out}");
    assert!(!out.contains("⚑ intent"), "{out}");
}

/// Bei 120 Spalten (geteilt) bekommt der Betreff mindestens 30 Zeichen, und
/// die Status-Spalte bleibt vollständig; bei 80 und 160 ebenso vollständig.
#[test]
fn c32_the_subject_gets_thirty_columns_at_120() {
    let (dir, _, _, _) = history();
    let long = "feat(sort): a stable sort that keeps equal keys in input order";
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", long]);
    let head: CommitId = git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap();
    let repo = Repo::open(dir.path()).unwrap();
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(sid('c'), session("Write the docs", "2026-07-25T15:00:00Z"));
    let mut links = BTreeMap::new();
    links.insert(head, vec![sid('a'), sid('c')]);
    let inspection = Inspection::from_index(Index::from_parts(sessions, links), Vec::new(), "demo");
    let mut app = opened(&repo, inspection);
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = list_lines(&lines_at(&mut app, w, h));
        let out = lines.join("\n");
        let row = lines
            .iter()
            .find(|l| l.contains("feat(sort): a stable"))
            .unwrap_or_else(|| panic!("{w}: {out}"));
        assert!(row.contains("· unsealed"), "{w}: {out}");
        assert!(row.contains("◉2"), "{w}: {out}");
        if w >= 120 {
            let shown: String = long.chars().take(30).collect();
            assert!(row.contains(&shown), "{w}: {out}");
        }
    }
}

/// Ein Auswahlbalken über die volle Zeile (beide Zeilen beim Agent-Commit):
/// kein Span der Auswahl hat einen eigenen Hintergrund.
#[test]
fn c32_the_selection_is_one_bar() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text = |y: u16| -> String {
        (0..160)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    // Die Zeile in der Liste — das Detail rechts nennt den Betreff auch.
    let y = (0..40)
        .find(|y| {
            let row = text(*y);
            row.split("││")
                .next()
                .unwrap_or(&row)
                .contains("docs: two sessions")
        })
        .unwrap();
    // Die Liste endet an ihrer rechten Rahmenkante (das zweite `│`).
    let edge = text(y)
        .chars()
        .enumerate()
        .filter(|(_, c)| *c == '│')
        .nth(1)
        .map(|(x, _)| x as u16)
        .unwrap();
    for row in [y, y + 1] {
        for x in 1..edge {
            let cell = &buffer[(x, row)];
            assert_eq!(
                cell.bg,
                ratatui::style::Color::Reset,
                "bg at {x},{row}: {}",
                text(row)
            );
            assert!(
                cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                "cursor at {x},{row}: {}",
                text(row)
            );
        }
    }
    // Keine ▌-Fragmente.
    assert!(!(0..40).any(|y| text(y).contains('▌')));
}

/// Review „open" ist der Normalzustand und leise; ein freigegebener Commit
/// sagt es — mit dem Glyph des Registers (`⊕`, nicht `✓`: `✓` heißt
/// „Prüfung bestanden").
#[test]
fn c32_open_is_quiet_and_approved_is_said() {
    let (_dir, repo, commits, _) = history();
    let mut sessions = BTreeMap::new();
    sessions.insert(
        sid('a'),
        session("Fix retry handling", "2026-07-25T14:10:00Z"),
    );
    sessions.insert(sid('c'), session("Write the docs", "2026-07-25T15:00:00Z"));
    let mut links = BTreeMap::new();
    links.insert(commits[2], vec![sid('a')]);
    links.insert(commits[1], vec![sid('c')]);
    let review = minds_core::Review::new(
        minds_core::Subject::Session(sid('a').to_string()),
        minds_core::Decision::Approve,
        "pd",
        "ok",
        Some("2026-07-26T00:00:00Z".into()),
    );
    let inspection =
        Inspection::from_index(Index::from_parts(sessions, links), vec![review], "demo");
    let mut app = opened(&repo, inspection);
    let lines = list_lines(&lines_at(&mut app, 160, 40));
    let out = lines.join("\n");
    assert!(!out.contains("⚠ open"), "{out}");
    let approved = lines
        .iter()
        .find(|l| l.contains("docs: two sessions"))
        .unwrap();
    assert!(approved.contains("⊕ approved"), "{out}");
}

// ---------------------------------------------------------------------
// C3.3 · Filter a: nur Commits mit Agent-Session
// ---------------------------------------------------------------------

/// Die Historie mit einem menschlichen Commit obenauf (HEAD) — damit der
/// Filter auch HEAD ausblendet und die Trennzeile ihn nennen muss.
fn human_on_top() -> (tempfile::TempDir, Repo, Vec<CommitId>, Inspection) {
    let (dir, _, mut commits, inspection) = history();
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "chore: by hand"],
    );
    commits.push(git(dir.path(), &["rev-parse", "HEAD"]).parse().unwrap());
    let repo = Repo::open(dir.path()).unwrap();
    (dir, repo, commits, inspection)
}

fn agent_only(app: &App) -> bool {
    app.overview.as_ref().unwrap().agent_only
}

/// `a` schaltet hin und zurück; der Titel nennt Filter und N of M, rechts
/// steht, was `a` jetzt bewirkt.
#[test]
fn c33_a_toggles_there_and_back() {
    let (_dir, repo, _, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains("a: agent sessions only"), "{out}");
    app.reduce(Action::AgentFilter);
    assert!(agent_only(&app));
    let lines = lines_at(&mut app, 160, 40);
    let out = lines.join("\n");
    assert!(
        lines[2].contains("OVERVIEW · agent sessions · 2 of 4 commits"),
        "{out}"
    );
    assert!(lines[2].contains("a: all"), "{out}");
    assert!(!out.contains("init: empty"), "{out}");
    assert!(!out.contains("chore: by hand"), "{out}");
    app.reduce(Action::AgentFilter);
    assert!(!agent_only(&app));
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains("init: empty"), "{out}");
    assert!(out.contains("OVERVIEW · 4 commits"), "{out}");
}

/// Esc setzt den Filter zurück (B1: Overlay → Suche → Filter → Ebene).
#[test]
fn c33_esc_resets_the_filter() {
    let (_dir, repo, _, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::AgentFilter);
    app.reduce(Action::SearchStart);
    app.reduce(Action::SearchInput('f'));
    app.reduce(Action::SearchCommit);
    // Erst die Suche, dann der Agent-Filter.
    app.reduce(Action::Back);
    assert!(app.overview.as_ref().unwrap().filter.is_empty());
    assert!(agent_only(&app));
    app.reduce(Action::Back);
    assert!(!agent_only(&app));
    assert!(!app.quit, "Esc never quits");
}

/// Ausgeblendetes wird zusammengefasst, nie spurlos entfernt: eine gedimmte
/// Zeile je Lauf, mit Zahl und den Refs darin — HEAD bleibt auffindbar.
/// Gezeigte + ausgeblendete Commits ergeben die Gesamtzahl (C3.1).
#[test]
fn c33_separators_carry_count_and_refs_and_the_sum_holds() {
    let (_dir, repo, _, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::AgentFilter);
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = list_lines(&lines_at(&mut app, w, h));
        let out = lines.join("\n");
        let top = lines
            .iter()
            .find(|l| l.contains("┆ 1 commit hidden"))
            .unwrap_or_else(|| panic!("{w}: {out}"));
        assert!(top.contains("HEAD"), "{w}: {out}");
        let hidden: usize = lines
            .iter()
            .filter_map(|l| {
                let at = l.find("┆ ")?;
                l[at + "┆ ".len()..]
                    .split(' ')
                    .next()?
                    .parse::<usize>()
                    .ok()
            })
            .sum();
        let state = app.overview.as_ref().unwrap();
        let data = state.data.as_ref().unwrap();
        let shown_commits = state
            .shown()
            .iter()
            .filter(|&&i| i >= data.wip.len())
            .count();
        assert_eq!(hidden, 2, "{w}: {out}");
        assert_eq!(shown_commits + hidden, data.counts().commits, "{w}: {out}");
    }
}

/// Die Auswahl springt auf den nächsten sichtbaren Commit — HEAD war ein
/// menschlicher Commit.
#[test]
fn c33_the_selection_stays_on_a_visible_commit() {
    let (_dir, repo, commits, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    let selected = |app: &App| match app.overview.as_ref().unwrap().selected() {
        Some(crate::overview::Selected::Commit(row)) => Some(row.id),
        _ => None,
    };
    assert_eq!(selected(&app), Some(commits[3]), "starts on HEAD");
    app.reduce(Action::AgentFilter);
    assert_eq!(selected(&app), Some(commits[2]));
    let state = app.overview.as_ref().unwrap();
    assert!(state.shown().contains(&state.cursor));
    // ↑↓ bleibt auf gezeigten Zeilen.
    app.reduce(Action::Down);
    assert_eq!(selected(&app), Some(commits[1]));
    app.reduce(Action::Down);
    assert_eq!(
        selected(&app),
        Some(commits[1]),
        "the human commit stays hidden"
    );
}

/// Der Filter bleibt beim Tab-Wechsel und beim Neuladen erhalten.
#[test]
fn c33_the_filter_survives_tabs_and_reloads() {
    let (_dir, repo, _, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::AgentFilter);
    app.reduce(Action::TabTo(1));
    app.reduce(Action::TabTo(0));
    assert!(agent_only(&app));
    let again = app.inspection.clone();
    app.reload(Ok(again), None, now());
    app.reduce(Action::TabTo(1));
    app.reduce(Action::TabTo(0));
    assert!(agent_only(&app));
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains("agent sessions · 2 of 4 commits"), "{out}");
}

/// Keine Agent-Session in der Historie: ein Leerzustand, der sagt, was los
/// ist und was hilft — keine leere Liste.
#[test]
fn c33_no_agent_session_says_so() {
    let (_dir, repo, _, _) = history();
    let inspection = Inspection::from_index(
        Index::from_parts(BTreeMap::new(), BTreeMap::new()),
        Vec::new(),
        "demo",
    );
    let mut app = opened(&repo, inspection);
    app.reduce(Action::AgentFilter);
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(
        out.contains("No commit with an agent session in the 3 commits read. a shows all."),
        "{out}"
    );
    assert!(out.contains("┆ 3 commits hidden"), "{out}");
}

/// `a` außerhalb der Übersicht tut nicht still nichts.
#[test]
fn c33_a_elsewhere_says_where_it_works() {
    let (_dir, repo, _, inspection) = human_on_top();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::TabTo(1));
    app.reduce(Action::AgentFilter);
    assert!(!agent_only(&app));
    assert!(
        app.closed
            .as_deref()
            .is_some_and(|c| c.contains("Overview"))
    );
}

// ---------------------------------------------------------------------
// C3.4 · Detail: Auftrag, Sessions, Beleg
// ---------------------------------------------------------------------

/// Eine Quelle für das Detail: Anker und Urteil, wie vorgegeben.
struct Detailed {
    detail: Option<crate::IntentDetail>,
    verdict: Option<crate::VerifyVerdict>,
}

impl crate::Source for Detailed {
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
                detail: self
                    .detail
                    .clone()
                    .ok_or_else(|| "not in the store".to_string()),
            })
            .collect::<Vec<_>>();
        Ok(crate::IntentList {
            total: entries.len(),
            entries,
            skipped: 0,
        })
    }
    fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
        match self.verdict {
            Some(verdict) => Ok(crate::CommitVerify {
                verdict: Ok(verdict),
                sessions: Vec::new(),
                out_of_scope: None,
                out_of_scope_paths: Vec::new(),
                scope_note: None,
                conformance: None,
            }),
            None => Err("minds verify not found".into()),
        }
    }
}

fn anchor_detail(proven: bool) -> crate::IntentDetail {
    crate::IntentDetail {
        source: "file:docs/anforderung.md@abc".into(),
        content: minds_core::ContentHash::from_bytes([2; 32]),
        scope: vec!["src/sort.rs".into(), "tests/**".into()],
        version: Some("HEAD version".into()),
        proof: if proven {
            Ok(())
        } else {
            Err("snapshot does not match the anchor".into())
        },
        signature: minds_reader::assurance::IntentSignature::Unsigned,
        snapshot_len: 40,
        snapshot: proven.then(|| vec!["Sort stably.".into(), "Keep equal keys in order.".into()]),
        snapshot_clipped: false,
    }
}

/// Der Agent-Commit mit Anker (`feat(sort): agent work`) gewählt.
fn on_agent_commit<'r>(repo: &'r Repo, inspection: Inspection, commit: CommitId) -> App<'r> {
    let mut app = opened(repo, inspection);
    app.overview.as_mut().unwrap().select_commit(commit);
    app
}

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

/// Keine Tastenhinweise im Detail — Tasten stehen nur in der Fußzeile.
/// Der Kopf: Kurz-Id + Betreff, darunter leise Branches, Autor, Datum UTC.
#[test]
fn c34_the_detail_leads_with_the_commit_and_names_no_keys() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[2]);
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let detail = detail_text(&mut app, w, h);
        assert!(!detail.contains("Enter: how it was made"), "{w}: {detail}");
        assert!(
            !detail.contains("Enter: the session's graph"),
            "{w}: {detail}"
        );
        assert!(!detail.contains("Enter: choose a session"), "{w}: {detail}");
        let short = &commits[2].to_string()[..7];
        assert!(
            detail.contains(&format!("{short} · docs: two sessions")),
            "{w}: {detail}"
        );
        assert!(
            detail.contains("on main · Patrick Döring · "),
            "{w}: {detail}"
        );
        assert!(detail.contains(" UTC"), "{w}: {detail}");
        assert!(detail.contains("SESSIONS (2)"), "{w}: {detail}");
        assert!(detail.contains("EVIDENCE"), "{w}: {detail}");
        for axis in [
            "INTEGRITY",
            "COVERAGE",
            "INTERPRETATION",
            "REQUIREMENT",
            "REVIEW",
        ] {
            assert!(detail.contains(axis), "{w}: {axis} {detail}");
        }
    }
}

/// Belegter Anker: Space zeigt die Snapshot-Zeilen. Unbelegter: Der Text
/// ist nie sichtbar, der Grund schon.
#[test]
fn c34_space_shows_the_requirement_only_when_proven() {
    for proven in [true, false] {
        let (_dir, repo, commits, inspection) = history();
        let mut app = on_agent_commit(&repo, inspection, commits[1]);
        let source = Detailed {
            detail: Some(anchor_detail(proven)),
            verdict: None,
        };
        while app.fill_overview(&source) {}
        let before = detail_text(&mut app, 160, 40);
        assert!(before.contains("REQUIREMENT"), "{before}");
        assert!(before.contains("⚑ anchor 01010101"), "{before}");
        assert!(before.contains("file:docs/anforderung.md@abc"), "{before}");
        assert!(before.contains("scope  src/sort.rs, tests/**"), "{before}");
        assert!(!before.contains("Sort stably."), "{before}");
        app.reduce(Action::Space);
        let after = detail_text(&mut app, 160, 40);
        if proven {
            assert!(before.contains("Space: show text (2 lines)"), "{before}");
            assert!(after.contains("Sort stably."), "{after}");
            assert!(after.contains("Keep equal keys in order."), "{after}");
        } else {
            assert!(!after.contains("Sort stably."), "{after}");
            assert!(
                after.contains("text hidden — snapshot does not match the anchor"),
                "{after}"
            );
        }
    }
}

/// EVIDENCE vor dem Urteil: `… checking`, kein ✓ in INTEGRITY. Danach die
/// Wörter von `minds verify`; TAMPERED ist ✗ und rot; „not signed" ist ◦,
/// nie ✗ und nie rot.
#[test]
fn c34_evidence_waits_for_the_verdict_and_never_alarms_wrongly() {
    let integrity = |detail: &str| {
        detail
            .lines()
            .find(|l| l.contains("INTEGRITY"))
            .unwrap_or_else(|| panic!("{detail}"))
            .to_string()
    };
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[1]);
    let detail = detail_text(&mut app, 160, 40);
    assert!(integrity(&detail).contains("… checking"), "{detail}");
    assert!(!integrity(&detail).contains('✓'), "{detail}");
    assert!(!detail.contains("VERIFIED"), "{detail}");

    let source = Detailed {
        detail: Some(anchor_detail(true)),
        verdict: Some(crate::VerifyVerdict::Verified),
    };
    while app.fill_overview(&source) {}
    assert!(app.fill_overview_verify(&source));
    let detail = detail_text(&mut app, 160, 40);
    assert!(integrity(&detail).contains("✓ VERIFIED"), "{detail}");
    assert!(detail.contains("checked "), "{detail}");
    let requirement = detail
        .lines()
        .rfind(|l| l.contains("REQUIREMENT"))
        .unwrap()
        .to_string();
    assert!(requirement.contains("✓ proven"), "{detail}");
    assert!(requirement.contains("◦ not signed"), "{detail}");
    assert!(!requirement.contains('✗'), "{detail}");
    // Die CLI nimmt nur die volle Id — kein Kurz-Befehl, den es nicht gibt.
    assert!(
        detail.contains("→ minds intent sign <id> (Intent tab, i)"),
        "{detail}"
    );
    // „not signed" nie rot.
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let red = crate::theme::tampered().2.fg;
    for y in 0..40 {
        let row: String = (0..160)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(at) = row.find("◦ not signed") {
            let x = row[..at].chars().count() as u16;
            assert_ne!(buffer[(x, y)].fg, red.unwrap(), "{row}");
        }
    }

    // TAMPERED: ✗ und rot.
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[1]);
    let source = Detailed {
        detail: None,
        verdict: Some(crate::VerifyVerdict::Tampered),
    };
    assert!(app.fill_overview_verify(&source));
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let y = (0..40)
        .find(|y| {
            (0..160)
                .map(|x| buffer[(x, *y)].symbol().to_string())
                .collect::<String>()
                .contains("✗ TAMPERED")
        })
        .expect("TAMPERED in INTEGRITY");
    let row: String = (0..160)
        .map(|x| buffer[(x, y)].symbol().to_string())
        .collect();
    assert!(row.contains("INTEGRITY"), "{row}");
    let x = row[..row.find("✗ TAMPERED").unwrap()].chars().count() as u16;
    assert_eq!(buffer[(x, y)].fg, red.unwrap());

    // Ohne Urteil (Fehler): „not checked" mit Grund, kein ✓.
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[1]);
    let source = Detailed {
        detail: None,
        verdict: None,
    };
    assert!(app.fill_overview_verify(&source));
    let detail = detail_text(&mut app, 160, 40);
    assert!(
        integrity(&detail).contains("not checked — minds verify not found"),
        "{detail}"
    );
    assert!(!integrity(&detail).contains('✓'), "{detail}");
}

/// Die Zahl unerklärter Zeilen im Detail ist die ◦n des Changes-Tabs für
/// denselben Commit.
#[test]
fn c34_unexplained_lines_match_the_changes_tab() {
    let (_dir, repo, inspection) = reconciled(true);
    let mut app = App::new(inspection, &repo, None);
    app.open_overview();
    let source = Detailed {
        detail: None,
        verdict: None,
    };
    while app.fill_overview(&source) {}
    let state = app.overview.as_ref().unwrap();
    let commit = state.agent_commit().expect("agent commit").id;
    let detail = detail_text(&mut app, 160, 40);
    let set = app.inspection.changes(&repo, commit).unwrap();
    let per_file: usize = set.files.iter().map(|f| f.unexplained()).sum();
    assert!(per_file > 0);
    assert!(
        detail.contains(&format!(
            "◦ {per_file} of {} added lines unexplained",
            set.added()
        )),
        "{detail}"
    );
    assert!(detail.contains("→ d, then n"), "{detail}");
    // Der Changes-Tab zeigt dieselbe Zahl je Datei.
    app.open_changes(Some(commit));
    let out = lines_at(&mut app, 160, 40).join("\n");
    assert!(out.contains(&format!("◦{per_file}")), "{out}");
}

/// Ein Commit ohne Agent-Session: Kopf und ein Satz — kein Block voller ·.
#[test]
fn c34_a_commit_without_an_agent_session_says_so() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[0]);
    let detail = detail_text(&mut app, 160, 40);
    assert!(
        detail.contains("No agent session linked to this commit."),
        "{detail}"
    );
    for gone in ["REQUIREMENT", "SESSIONS", "EVIDENCE", "INTEGRITY"] {
        assert!(!detail.contains(gone), "{gone}: {detail}");
    }
}

/// Breite: Das Detail bekommt mindestens 60 Spalten; unter 100 nur die
/// Liste, das Detail per Enter als eigene Ebene, Esc zurück.
#[test]
fn c34_the_detail_gets_sixty_columns_and_is_a_level_when_narrow() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[1]);
    for (w, h) in [(120u16, 30u16), (160, 40)] {
        let lines = lines_at(&mut app, w, h);
        let title = &lines[2];
        let at = title
            .find("┐┌")
            .unwrap_or_else(|| panic!("{}", lines.join("\n")));
        let list_w = title[..at].chars().count() + 1;
        assert!(usize::from(w) - list_w >= 60, "{w}: {}", lines.join("\n"));
    }
    let lines = lines_at(&mut app, 80, 24);
    assert!(!lines.iter().any(|l| l.contains("DETAIL")));
    app.reduce(Action::Enter);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(out.contains("DETAIL"), "{out}");
    assert!(out.contains("EVIDENCE"), "{out}");
    app.reduce(Action::Back);
    let out = lines_at(&mut app, 80, 24).join("\n");
    assert!(!out.contains("DETAIL"), "{out}");
    assert!(out.contains("OVERVIEW"), "{out}");
}

/// `◦` heißt im Register „open" — offen, kein Fehler (C3.4).
#[test]
fn c34_the_open_glyph_is_registered_as_open() {
    let entry = crate::theme::REGISTER
        .iter()
        .find(|e| e.symbol == "◦")
        .unwrap();
    assert_eq!(entry.meaning, "open");
    assert_ne!(entry.meaning, "not observed");
    let (glyph, word, style) = crate::theme::open();
    assert_eq!((glyph, word), ("◦", "open"));
    assert_ne!(style.fg, crate::theme::tampered().2.fg, "open is never red");
}

// ---------------------------------------------------------------------
// C3.5 · Kopf und Fußzeile der Overview
// ---------------------------------------------------------------------

/// Kopf, Zeile 1: der Branch-Kurzname — `main`, nicht `refs/heads/main`.
#[test]
fn c35_the_header_names_the_short_branch() {
    use crate::view::branch_label;
    assert_eq!(branch_label(Some("refs/heads/main")), "main");
    assert_eq!(
        branch_label(Some("refs/heads/feat/stable-sort")),
        "feat/stable-sort"
    );
    assert_eq!(branch_label(Some("main")), "main");
    assert_eq!(branch_label(None), "(detached)");
}

/// Fußzeile, Zeile 1: Kurz-Id des gewählten Commits + Filterzustand — kein
/// Erklärsatz, keine Backticks. Zeile 2: die Aktionen, `? help  q quit`
/// rechts fest — in 80, 120 und 160 Spalten.
#[test]
fn c35_the_footer_says_where_you_are_and_what_you_can_do() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent_commit(&repo, inspection, commits[1]);
    let short = &commits[1].to_string()[..7];
    for (w, h) in [(80u16, 24u16), (120, 30), (160, 40)] {
        let lines = lines_at(&mut app, w, h);
        let context = &lines[usize::from(h) - 2];
        let keys = &lines[usize::from(h) - 1];
        let out = lines.join("\n");
        assert!(
            context.contains(&format!("{short} · all commits")),
            "{w}: {out}"
        );
        assert!(!context.contains('`'), "{w}: {out}");
        assert!(!context.contains("Unsealed:"), "{w}: {out}");
        assert!(keys.trim_end().ends_with("? help  q quit"), "{w}: {out}");
        assert!(keys.contains("↑↓ commit"), "{w}: {out}");
        // Priorisiert gekürzt: bei 160 alle sechs, darunter von hinten.
        let all = [
            "Enter how it was made",
            "a agent only/all",
            "w why",
            "v verify",
            "d diff",
        ];
        let want = if w >= 160 { &all[..] } else { &all[..1] };
        for key in want {
            assert!(keys.contains(key), "{w}: {key}: {out}");
        }
        // Space nur, wenn es etwas aufzuklappen gibt.
        assert!(!keys.contains("Space requirement"), "{w}: {out}");
    }
    app.reduce(Action::AgentFilter);
    let lines = lines_at(&mut app, 160, 40);
    assert!(
        lines[38].contains(&format!("{short} · showing agent sessions only")),
        "{}",
        lines.join("\n")
    );
    // Belegter Anker: Space steht in den Aktionen.
    let source = Detailed {
        detail: Some(anchor_detail(true)),
        verdict: None,
    };
    while app.fill_overview(&source) {}
    let lines = lines_at(&mut app, 160, 40);
    assert!(
        lines[39].contains("Space requirement"),
        "{}",
        lines.join("\n")
    );
}
