//! Batch 2 · Paket 2: Der Navigationsverlauf stapelt denselben Ursprung
//! nie — ein Tab-Wechsel per Taste beendet den Sprung, ein Push ohne neuen
//! Ort unterbleibt, der Breadcrumb bleibt kurz.

use super::*;

/// Die Kopfzeile 2 bei 160×40: der Breadcrumb.
fn crumb(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal
        .backend()
        .to_string()
        .lines()
        .nth(1)
        .unwrap()
        .trim_matches('"')
        .trim_end()
        .to_string()
}

/// Wählt in der Overview die erste Commit-Zeile (die Session ohne Commit
/// steht oben, `d` braucht einen Commit).
fn select_a_commit(app: &mut App) -> CommitId {
    for _ in 0..20 {
        if let Some(crate::overview::Selected::Commit(row)) =
            app.overview.as_ref().and_then(|s| s.selected())
        {
            return row.id;
        }
        app.reduce(Action::Down);
    }
    panic!("no commit row");
}

fn short(id: CommitId) -> String {
    id.to_string().chars().take(7).collect()
}

/// Das AC des Auftrags: `d, F1, d, F1, d` ergibt
/// `Overview › <id> › Changes › <id>` — nicht `Overview › <id> › Overview ›
/// <id> › …`.
#[test]
fn nav_a_tab_key_back_to_the_origin_ends_the_jump() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let id = short(select_a_commit(&mut app));
    for _ in 0..2 {
        app.reduce(Action::Diff);
        assert_eq!(app.tab, Tab::Changes);
        app.reduce(Action::TabTo(0)); // F1
        assert_eq!(app.tab, Tab::Overview);
        assert!(app.history.is_empty(), "{:?}", app.breadcrumb());
    }
    app.reduce(Action::Diff);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.history.len(), 1);
    assert_eq!(
        app.breadcrumb(),
        vec!["Overview", &id, "Changes", &id],
        "{:?}",
        app.breadcrumb()
    );
    let line = crumb(&mut app);
    assert!(
        line.starts_with(&format!("Overview › {id} › Changes › {id}")),
        "{line}"
    );
    assert_eq!(line.matches("Overview").count(), 1, "{line}");
}

/// Die Zifferntaste wirkt wie die F-Taste: `d, 1` räumt den Ursprung weg,
/// auch wenn er nicht der letzte Eintrag ist.
#[test]
fn nav_a_tab_key_removes_every_entry_from_the_origin_on() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    select_a_commit(&mut app);
    app.reduce(Action::Diff); // Overview → Changes
    app.reduce(Action::Verify); // Changes → Verify
    assert_eq!(app.tab, Tab::Verify);
    assert_eq!(app.history.len(), 2, "{:?}", app.breadcrumb());
    app.reduce(Action::TabTo(3)); // F4 Changes: der zweite Ursprung
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.history.len(), 1, "{:?}", app.breadcrumb());
    assert_eq!(app.history[0].tab, Tab::Overview);
    app.reduce(Action::TabTo(0)); // F1 Overview: der erste
    assert!(app.history.is_empty(), "{:?}", app.breadcrumb());
}

/// `d, Esc` leert den Verlauf: Esc auf der Ebene des Sprungs kehrt zurück.
#[test]
fn nav_d_then_esc_leaves_no_history() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    select_a_commit(&mut app);
    app.reduce(Action::Diff);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.history.len(), 1);
    // Esc geht den genommenen Weg zurück: der Sprung `d` führte aus der
    // Übersicht direkt in den Diff, also beendet das erste Esc den Sprung —
    // Paket 2 (`d, Esc` leert den Verlauf) und Paket 3 („Esc files" nach
    // FILES › Enter) gelten beide.
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Overview);
    assert!(app.history.is_empty(), "{:?}", app.breadcrumb());
    assert_eq!(app.breadcrumb().len(), 2, "{:?}", app.breadcrumb());
}

/// Regel 2: Ein Ursprung, der dem letzten Eintrag gleicht (Tab + Ort), wird
/// nicht erneut abgelegt.
#[test]
fn nav_the_same_origin_is_not_pushed_twice() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    select_a_commit(&mut app);
    let origin = app.snapshot();
    let mut again = origin.clone();
    again.target = Tab::Changes;
    app.remember(origin);
    app.remember(again);
    assert_eq!(app.history.len(), 1, "{:?}", app.breadcrumb());
}

/// Regel 3: Der Breadcrumb zeigt nie zweimal denselben Ort hintereinander —
/// auch nicht, wenn der aktuelle Ort dem letzten Ursprung gleicht.
#[test]
fn nav_the_breadcrumb_never_repeats_a_place() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let commit = select_a_commit(&mut app);
    let id = short(commit);
    app.active_commit = Some(commit);
    let mut origin = app.snapshot();
    origin.target = Tab::Changes;
    app.history.push(origin);
    // Der aktuelle Ort ist der Ursprung selbst.
    assert_eq!(app.breadcrumb(), vec!["Overview", &id]);
}

/// Regel 4: höchstens acht Einträge, die ältesten fallen weg.
#[test]
fn nav_the_history_keeps_at_most_eight_entries() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    select_a_commit(&mut app);
    app.reduce(Action::Diff); // Overview → Changes
    for _ in 0..6 {
        app.reduce(Action::Verify); // Changes → Verify
        assert_eq!(app.tab, Tab::Verify);
        app.reduce(Action::Diff); // Verify → Changes
        assert_eq!(app.tab, Tab::Changes);
    }
    assert_eq!(app.history.len(), 8, "{:?}", app.breadcrumb());
    // Der älteste (die Overview) ist weg, der jüngste bleibt.
    assert!(app.history.iter().all(|e| e.tab != Tab::Overview));
    assert_eq!(app.history.last().unwrap().tab, Tab::Verify);
}
