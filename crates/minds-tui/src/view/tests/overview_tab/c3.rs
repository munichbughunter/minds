//! C3 · Overview als Einstieg: Betreff vorn, Zustände rechtsbündig, das
//! Detail beantwortet „Kann ich diesem Commit trauen?" samt nächster Aktion,
//! die Statistik im Titel, unter 100 Spalten nur die Liste.

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

#[test]
fn c3_the_subject_leads_and_states_stand_right() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let lines = lines_at(&mut app, 160, 40);
    let row = lines
        .iter()
        .find(|l| l.contains("feat(sort): agent work"))
        .expect("agent row");
    let subject = row.find("feat(sort): agent work").unwrap();
    let state = row.find("unsealed").expect("seal state pill");
    assert!(subject < state, "subject first: {row}");
    // Rechtsbündig: Die Zustände stehen in einer festen Spalte, davor
    // steht Luft; die Zeile endet mit der Zeit (C3.2: Status · Refs ·
    // Zeit), nicht mehr mit einer Pille.
    let list_part = &row[subject..row[subject..].find('│').map_or(row.len(), |b| b + subject)];
    assert!(
        list_part.trim_end().ends_with("just now"),
        "right-aligned: {row}"
    );
    assert!(
        !list_part.trim_end().ends_with("⚑ intent"),
        "right-aligned: {row}"
    );
    assert!(
        list_part.contains("   · unsealed ⚑"),
        "padding before the states: {row}"
    );
    // Die Zahlen stehen im Titel der Übersicht — gezählt über die Zeilen
    // (C3.1), nicht mehr die globale Statistik.
    assert!(lines[2].contains("with an agent session"), "{}", lines[2]);
    assert!(!lines[2].contains("Sessions"), "{}", lines[2]);
}

#[test]
fn c3_the_detail_names_the_next_action() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    while !matches!(
        app.overview.as_ref().and_then(|s| s.selected()),
        Some(crate::overview::Selected::Commit(_))
    ) {
        app.reduce(Action::Down);
    }
    let lines = lines_at(&mut app, 160, 40);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Enter: how it was made · v: verify · d: diff")),
        "{}",
        lines.join("\n")
    );
}

#[test]
fn c3_below_100_columns_only_the_list() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let lines = lines_at(&mut app, 90, 30);
    assert!(
        !lines.iter().any(|l| l.contains("DETAIL")),
        "{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|l| l.contains("OVERVIEW")),
        "{}",
        lines.join("\n")
    );
    // Der Fokus rechts (Enter auf einem Commit mit mehreren Sessions) zeigt
    // das Detail statt der Liste.
    if let Some(state) = app.overview.as_mut() {
        state.focus = crate::overview::OverviewFocus::Sessions;
    }
    let lines = lines_at(&mut app, 90, 30);
    assert!(
        lines.iter().any(|l| l.contains("DETAIL")),
        "{}",
        lines.join("\n")
    );
    let lines = lines_at(&mut app, 160, 40);
    assert!(lines.iter().any(|l| l.contains("DETAIL")) && lines[2].contains("OVERVIEW"));
}

/// Die Startauswahl ist HEAD — nicht zwingend die erste Zeile (C3).
#[test]
fn c3_the_overview_starts_on_head() {
    let (_dir, repo, commits, inspection) = history();
    let app = opened(&repo, inspection);
    match app.overview.as_ref().and_then(|s| s.selected()) {
        Some(crate::overview::Selected::Commit(row)) => assert_eq!(row.id, commits[2]),
        _ => panic!("HEAD is selected, not the session in progress"),
    }
    assert_ne!(
        app.overview.as_ref().unwrap().cursor,
        0,
        "row 0 is the WIP row"
    );
}
