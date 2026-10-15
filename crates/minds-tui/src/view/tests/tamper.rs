//! A5 · Manipulation sichtbar machen (Tamper-Banner).

use super::*;
use crate::theme;

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 30), (160, 40)];

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

fn buffer_sized(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

// ---------------------------------------------------------------------
// A5 · Manipulation sichtbar machen (Tamper-Banner)
// ---------------------------------------------------------------------

fn sealed_index(ids: &[char]) -> Index {
    let mut sessions = BTreeMap::new();
    for (i, c) in ids.iter().enumerate() {
        sessions.insert(
            sid(*c),
            session(&format!("Task {c}"), &format!("2026-07-25T14:1{i}:00Z")),
        );
    }
    let mut index = Index::from_parts(sessions, BTreeMap::new());
    for c in ids {
        let (seal_id, seal) = clean_seal(sid(*c));
        index = index.with_seals(sid(*c), vec![(seal_id, seal, false)]);
    }
    index
}

fn tampered(ids: &[char], bad: &[char]) -> Inspection {
    let mut index = sealed_index(ids);
    for c in bad {
        index = index.with_tampered_seal(sid(*c));
    }
    Inspection::from_index(index, vec![], "repo")
}

fn banner_of(out: &str) -> Option<String> {
    out.lines()
        .find(|l| l.contains("TAMPERED") && l.contains("Esc: dismiss"))
        .map(str::to_string)
}

#[test]
fn a5_a_session_turning_tampered_on_reload_raises_the_banner() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(tampered(&['a', 'b'], &[]), &repo, None);
        let out = render_sized(&mut app, w, h);
        assert!(banner_of(&out).is_none(), "{w}x{h}\n{out}");
        app.reduce(Action::Down); // Cursor weg von a
        app.reload(Ok(tampered(&['a', 'b'], &['a'])), None, now());
        let out = render_sized(&mut app, w, h);
        let banner = banner_of(&out).unwrap_or_else(|| panic!("{w}x{h}\n{out}"));
        assert!(banner.contains("✗ TAMPERED"), "{w}x{h}\n{out}");
        assert!(banner.contains("Enter"), "{w}x{h}\n{out}");
        // Direkt unter dem Kopf (zwei Zeilen), über die volle Breite.
        assert_eq!(out.lines().nth(2), Some(banner.as_str()), "{w}x{h}\n{out}");
        if w >= 120 {
            assert!(banner.contains("changed since"), "{w}x{h}\n{out}");
            assert!(banner.contains("b3-aaaaaaaa"), "{w}x{h}\n{out}");
        }
        // Rot und invertiert — Glyph und Wort tragen es auch ohne Farbe.
        let buffer = buffer_sized(&mut app, w, h);
        let cell = &buffer[(0, 2)];
        assert_eq!(cell.fg, theme::DELETE, "{w}x{h}");
        assert!(cell.modifier.contains(ratatui::style::Modifier::REVERSED));
        assert_eq!(buffer[(w - 1, 2)].fg, theme::DELETE, "{w}x{h}: full width");
    }
}

#[test]
fn a5_enter_on_the_banner_opens_the_evidence_of_the_session() {
    let (_dir, repo) = repo();
    let mut app = App::new(tampered(&['a', 'b'], &[]), &repo, None);
    app.reduce(Action::Down);
    app.reload(Ok(tampered(&['a', 'b'], &['a'])), None, now());
    app.reduce(Action::Enter);
    assert!(
        matches!(app.top(), Some(View::Evidence { id, .. }) if *id == sid('a')),
        "{:?}",
        app.top().map(std::mem::discriminant)
    );
    assert_eq!(app.selected().map(|c| c.id), Some(sid('a')));
    let out = render_sized(&mut app, 120, 30);
    assert!(banner_of(&out).is_none(), "{out}");
    assert!(out.contains(" SESSION TAMPERED "), "{out}");
}

#[test]
fn a5_esc_dismisses_the_banner_and_does_not_quit() {
    let (_dir, repo) = repo();
    let mut app = App::new(tampered(&['a', 'b'], &[]), &repo, None);
    app.reload(Ok(tampered(&['a', 'b'], &['b'])), None, now());
    app.reduce(Action::Back);
    assert!(!app.quit);
    let out = render_sized(&mut app, 120, 30);
    assert!(banner_of(&out).is_none(), "{out}");
    // Der rote Badge bleibt — nur das Banner ist gelesen.
    assert!(out.contains("✗ TAMPERED"), "{out}");
    // Ein weiteres Neuladen ohne neue Manipulation bringt es nicht zurück.
    app.reload(Ok(tampered(&['a', 'b'], &['b'])), None, now());
    let out = render_sized(&mut app, 120, 30);
    assert!(banner_of(&out).is_none(), "{out}");
}

#[test]
fn a5_several_tampered_sessions_are_counted_and_enter_opens_the_first() {
    let (_dir, repo) = repo();
    let mut app = App::new(tampered(&['a', 'b', 'c'], &[]), &repo, None);
    app.reload(
        Ok(tampered(&['a', 'b', 'c'], &['a', 'b', 'c'])),
        None,
        now(),
    );
    for (w, h) in SIZES {
        let out = render_sized(&mut app, w, h);
        let banner = banner_of(&out).unwrap_or_else(|| panic!("{w}x{h}\n{out}"));
        assert!(banner.contains("✗ 3 sessions TAMPERED"), "{w}x{h}\n{out}");
    }
    app.reduce(Action::Enter);
    let first = app.cards.iter().map(|c| c.id).next();
    assert!(matches!(app.top(), Some(View::Evidence { id, .. }) if Some(*id) == first));
}

#[test]
fn a5_tampered_at_start_shows_the_banner_too() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(tampered(&['a'], &['a']), &repo, None);
        let out = render_sized(&mut app, w, h);
        let banner = banner_of(&out).unwrap_or_else(|| panic!("{w}x{h}\n{out}"));
        assert!(banner.contains("✗ TAMPERED"), "{w}x{h}\n{out}");
        // Kopf, Fußzeile und Hilfe-Hinweis bleiben.
        assert!(out.contains("Sessions"), "{w}x{h}\n{out}");
    }
}
