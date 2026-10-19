//! K2 · Zwei fremde `open`-Sessions in einem frischen Repo: Eine Session
//! ohne Commit zeigt ihre Herkunft (`imported` / `captured` / `witnessed`)
//! und ihr Alter, damit verwaiste Sessions als solche erkennbar sind; das
//! Detail einer importierten nennt `minds forget`. Kein Rot — eine fremde
//! Session ist eine Lücke, kein Manipulationsbefund.

use minds_core::TranscriptStatus;
use ratatui::style::Color;

use super::*;

/// Eine Historie mit einem Commit und zwei Sessions ohne Commit: eine
/// importierte (kein Seal, Transkript gelesen — wie `minds import` sie
/// baut) und eine von den Hooks erfasste (Hook-Seal).
fn with_foreign_sessions() -> (tempfile::TempDir, Repo, Inspection) {
    let (dir, repo) = repo();
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "chore: init"],
    );

    let mut imported = session("Setze LOG-2417 um", "2026-07-20T10:00:00Z");
    imported.lineage.as_mut().unwrap().transcript = Some(TranscriptStatus::Read);
    let mut captured = session("Write the docs", "2026-07-19T10:00:00Z");
    captured.lineage.as_mut().unwrap().transcript = Some(TranscriptStatus::Read);

    let mut sessions = BTreeMap::new();
    sessions.insert(sid('e'), imported);
    sessions.insert(sid('c'), captured);
    let (seal_id, seal) = clean_seal(sid('c'));
    let index = Index::from_parts(sessions, BTreeMap::new())
        .with_seals(sid('c'), vec![(seal_id, seal, false)]);
    (dir, repo, Inspection::from_index(index, Vec::new(), "demo"))
}

/// Die Overview, die Uhr drei Tage nach dem Start der importierten Session.
fn opened_three_days_later<'r>(repo: &'r Repo, inspection: Inspection) -> App<'r> {
    let mut app = App::new(inspection, repo, None);
    let start = minds_metrics::epoch_seconds("2026-07-20T10:00:00Z").unwrap();
    app.now = std::time::UNIX_EPOCH + std::time::Duration::from_secs((start + 3 * 86_400) as u64);
    app.open_overview();
    app
}

/// Die Zeile einer Session ohne Commit nennt Herkunft und Alter:
/// `open · imported · 3d ago · no commit linked` — statt nur
/// `open · no commit linked yet` (Klasse 2: die Darstellung ändert sich
/// bewusst, die Absicht — nur sagen, was bekannt ist — bleibt).
#[test]
fn k2_the_list_names_origin_and_age() {
    let (_dir, repo, inspection) = with_foreign_sessions();
    let mut app = opened_three_days_later(&repo, inspection);
    let out = render(&mut app);
    assert!(
        out.contains("open · imported · 3d ago · no commit linked"),
        "{out}"
    );
    assert!(
        out.contains("open · captured · 4d ago · no commit linked"),
        "{out}"
    );
    assert!(!out.contains("no commit linked yet"), "{out}");
}

/// Das Detail einer importierten Session sagt, woher sie kommt und wie sie
/// wieder verschwindet — mit dem vollen `minds forget`-Befehl; bei einer
/// von den Hooks erfassten steht die Herkunft ohne den Befehl.
#[test]
fn k2_the_detail_names_the_origin_and_minds_forget_for_an_import() {
    let (_dir, repo, inspection) = with_foreign_sessions();
    let mut app = opened_three_days_later(&repo, inspection);
    // Start auf HEAD (C3), die Sessions ohne Commit stehen darüber — neueste
    // zuerst: die importierte (20.7.) über der erfassten (19.7.).
    app.reduce(Action::Home);
    let out = render(&mut app);
    assert!(
        out.contains("imported from the agent's transcript store"),
        "{out}"
    );
    assert!(
        out.contains(&format!("minds forget {}", sid('e'))),
        "the full id, as minds forget wants it: {out}"
    );
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(out.contains("captured by the agent's hooks"), "{out}");
    assert!(!out.contains("minds forget"), "{out}");
}

/// Herkunft und Alter stehen in jeder Breite, neben `? help`; nichts
/// daran ist rot — es ist kein Manipulationsbefund.
#[test]
fn k2_origin_fits_every_width_and_is_never_red() {
    let (_dir, repo, inspection) = with_foreign_sessions();
    let mut app = opened_three_days_later(&repo, inspection);
    for w in [80u16, 120, 160] {
        let out = render_at(&mut app, w);
        assert!(out.contains("imported · 3d ago"), "{w}: {out}");
        assert!(out.contains("? help"), "{w}: {out}");
    }
    let buffer = render_buffer(&mut app);
    let mut seen = false;
    for y in 0..buffer.area.height {
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(at) = line.find("imported") {
            seen = true;
            for x in at..at + "imported".len() {
                let cell = &buffer[(x as u16, y)];
                assert_ne!(cell.fg, Color::Red, "imported is never red: {line}");
                assert_ne!(
                    cell.fg,
                    crate::theme::DELETE,
                    "imported is never red: {line}"
                );
            }
        }
    }
    assert!(seen);
}
