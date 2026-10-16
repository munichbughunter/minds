//! B1 · Ein Esc-Modell für alle Tabs: 1. Overlay/Banner schließen →
//! 2. Suche leeren → 3. oberste Ebene schließen → 4. zurück zum Ursprung
//! (B2) → 5. sonst nichts. Esc beendet nie.

use super::*;
use crate::changes::Focus;
use crate::overview::OverviewFocus;

/// Ein Fall: Name, Aufbau, erwartete Wirkung von Esc.
type Case = (&'static str, fn(&mut App), fn(&App) -> bool);

fn cases() -> Vec<Case> {
    vec![
        (
            "help closes first",
            |app| {
                app.reduce(Action::TabTo(1));
                app.reduce(Action::Help);
            },
            |app| !app.help && app.tab == Tab::Sessions,
        ),
        (
            "typing a search: clear it",
            |app| {
                app.reduce(Action::TabTo(1));
                app.reduce(Action::SearchStart);
                app.reduce(Action::SearchInput('x'));
            },
            |app| !app.searching && app.query.is_empty(),
        ),
        (
            "a kept search: clear it",
            |app| {
                app.reduce(Action::TabTo(1));
                app.reduce(Action::SearchStart);
                app.reduce(Action::SearchInput('x'));
                app.reduce(Action::SearchCommit);
            },
            |app| app.query.is_empty() && app.tab == Tab::Sessions,
        ),
        (
            "a layer closes",
            |app| {
                app.reduce(Action::TabTo(1));
                app.reduce(Action::Enter);
            },
            |app| app.top().is_none() && app.tab == Tab::Sessions,
        ),
        (
            "sessions list: nothing",
            |app| app.reduce(Action::TabTo(1)),
            |app| app.tab == Tab::Sessions && app.top().is_none(),
        ),
        (
            "overview graph: nothing",
            |app| app.reduce(Action::TabTo(0)),
            |app| app.tab == Tab::Overview,
        ),
        (
            "overview sessions pane: back to the graph",
            |app| {
                app.reduce(Action::TabTo(0));
                if let Some(state) = app.overview.as_mut() {
                    state.focus = OverviewFocus::Sessions;
                }
            },
            |app| {
                app.tab == Tab::Overview
                    && app.overview.as_ref().unwrap().focus == OverviewFocus::Graph
            },
        ),
        (
            "verify: nothing",
            |app| app.reduce(Action::TabTo(2)),
            |app| app.tab == Tab::Verify,
        ),
        // Klasse 2 (Batch 2, Paket 3 / C9.1): Der Tab landet im Diff, nicht
        // in der Dateiliste — `Esc` führt aus dem Diff in die Liste, ohne
        // vorheriges `Enter`.
        (
            "changes diff: back to the files",
            |app| app.reduce(Action::TabTo(3)),
            |app| {
                app.tab == Tab::Changes
                    && app
                        .changes
                        .as_ref()
                        .is_none_or(|c| c.focus == Focus::Files && c.focus != Focus::Diff)
            },
        ),
        (
            "changes files: nothing",
            |app| {
                app.reduce(Action::TabTo(3));
                app.reduce(Action::Back);
            },
            |app| app.tab == Tab::Changes,
        ),
        (
            "intent: nothing",
            |app| app.reduce(Action::TabTo(4)),
            |app| app.tab == Tab::Intent,
        ),
    ]
}

#[test]
fn b1_esc_follows_one_model_in_every_tab_and_never_quits() {
    for (name, setup, expect) in cases() {
        let (_dir, repo, _, inspection) = history();
        let mut app = opened(&repo, inspection);
        setup(&mut app);
        app.reduce(Action::Back);
        assert!(!app.quit, "{name}: Esc must never quit");
        assert!(expect(&app), "{name}: tab {:?}", app.tab);
        // Ein zweites, drittes Esc beendet ebenso wenig.
        app.reduce(Action::Back);
        app.reduce(Action::Back);
        assert!(!app.quit, "{name}: Esc must never quit");
    }
    // Nur q und Ctrl-C beenden.
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::Quit);
    assert!(app.quit);
}
