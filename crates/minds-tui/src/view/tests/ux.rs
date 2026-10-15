//! Render-Proben der UX-Überarbeitung (docs/specs/tui-ux-auftrag.md) — je
//! Paket ein Abschnitt, jede Probe in den drei Referenzgrößen 80×24,
//! 120×30 und 160×40.

use super::*;
use crate::theme;
use ratatui::style::Color;

/// Die drei Referenzgrößen der Test-Konvention.
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

/// Die Vordergrundfarben aller Zellen, die `needle` in einer Zeile tragen.
fn colors_of(buffer: &ratatui::buffer::Buffer, needle: &str) -> Vec<Color> {
    let area = buffer.area;
    let mut found = Vec::new();
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(byte) = line.find(needle) {
            let start = line[..byte].chars().count() as u16;
            for x in start..start + needle.chars().count() as u16 {
                found.push(buffer[(x, y)].fg);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------
// A3 · Verworfene Ansätze und Constraints sichtbar machen
// ---------------------------------------------------------------------

const DISCARDED: [&str; 3] = [
    "Retry inside the HTTP client",
    "Circuit breaker via tower",
    "Fixed 1 s sleep between tries",
];

/// Eine Session mit mehrzeiligem Auftrag, zwei Constraints und drei
/// verworfenen Ansätzen — sonst wie `filled()`.
fn deliberate() -> Inspection {
    let mut s = session(
        "Fix retry handling\nOnly retry idempotent requests\nKeep the public API",
        "2026-07-25T14:10:00Z",
    );
    s.intent.constraints = vec!["no new dependency".into(), "max 3 retries".into()];
    s.intent.discarded = DISCARDED.iter().map(|d| d.to_string()).collect();
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    Inspection::from_index(
        Index::from_parts(sessions, commits),
        Vec::new(),
        "payment-service",
    )
}

#[test]
fn a3_why_lists_every_discarded_approach_as_a_claim() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(deliberate(), &repo, None);
        app.reduce(Action::Why);
        // Auf das INTENT-Glied: Session, Agent, Intent.
        app.reduce(Action::Down);
        app.reduce(Action::Down);
        let out = render_sized(&mut app, w, h);
        assert!(out.contains("Considered & discarded"), "{w}x{h}\n{out}");
        assert!(out.contains("Constraints"), "{w}x{h}\n{out}");
        assert!(out.contains("no new dependency"), "{w}x{h}\n{out}");
        for d in DISCARDED {
            // Bei 80 Spalten darf die Zeile mit … gekappt sein, der Anfang
            // muss lesbar bleiben.
            let head: String = d.chars().take(18).collect();
            assert!(out.contains(&head), "{w}x{h} {d}\n{out}");
        }
        assert!(!out.contains("Discarded: "), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        for color in colors_of(&buffer, "Circuit breaker") {
            assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        }
    }
}

#[test]
fn a3_graph_detail_shows_all_lines_of_the_intent() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(deliberate(), &repo, None);
        app.reduce(Action::Enter);
        let out = render_sized(&mut app, w, h);
        for d in DISCARDED {
            let head: String = d.chars().take(18).collect();
            assert!(out.contains(&head), "{w}x{h} {d}\n{out}");
        }
        assert!(out.contains("max 3 retries"), "{w}x{h}\n{out}");
        assert!(out.contains("Keep the public API"), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        for color in colors_of(&buffer, "Circuit breaker") {
            assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        }
    }
}

#[test]
fn a3_the_you_box_shows_the_request_and_counts_the_claims() {
    let (_dir, repo) = repo();
    let mut app = App::new(deliberate(), &repo, None);
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("Only retry idempotent requests"), "{out}");
    assert!(
        out.contains("◌ 3 discarded · 2 constraints — select YOU for details"),
        "{out}"
    );
}

#[test]
fn a3_empty_is_empty() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("Considered & discarded"), "{out}");
    assert!(!out.contains("Discarded: 0"), "{out}");
    app.reduce(Action::Back);
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("discarded ·"), "{out}");
}

// ---------------------------------------------------------------------
// A2 · Claim und Evidence in der Why-Kette trennen
// ---------------------------------------------------------------------

/// Die Zelle des Glied-Markers vor `word` (`"INTENT"` …) — Symbol und Farbe.
fn marker_of(buffer: &ratatui::buffer::Buffer, word: &str) -> Option<(String, Color)> {
    let area = buffer.area;
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        let chars: Vec<char> = line.chars().collect();
        let text: String = chars.iter().collect();
        if let Some(byte) = text.find(&format!(" {word}")) {
            let at = text[..byte].chars().count();
            // Ein Glied-Kopf: Marker, Leerzeichen, Wort — am Zeilenanfang
            // der Kette (Rahmen und Liste davor sind erlaubt).
            if at >= 1 && !chars[at - 1].is_whitespace() {
                let x = (at - 1) as u16;
                return Some((buffer[(x, y)].symbol().to_string(), buffer[(x, y)].fg));
            }
        }
    }
    None
}

#[test]
fn a2_the_intent_link_is_a_claim_never_a_check() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        app.reduce(Action::Why); // Session a: Trailer, belegt
        let out = render_sized(&mut app, w, h);
        assert!(out.contains("◌ INTENT"), "{w}x{h}\n{out}");
        assert!(!out.contains("✓ INTENT"), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        let (glyph, color) = marker_of(&buffer, "INTENT").expect("INTENT link");
        assert_eq!(glyph, "◌", "{w}x{h}");
        assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        assert_eq!(color, theme::claim().2.fg.unwrap(), "{w}x{h}");
        // Ein belegtes Glied bleibt ✓ in OK.
        let (glyph, color) = marker_of(&buffer, "EVIDENCE").expect("EVIDENCE link");
        assert_eq!((glyph.as_str(), color), ("✓", theme::OK), "{w}x{h}");
    }
}

#[test]
fn a2_no_gap_never_includes_the_claims() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("✓ no gap in the evidence links"), "{out}");
    assert!(out.contains("intent is a recorded claim"), "{out}");
    assert!(!out.contains("every link is attested"), "{out}");
    assert!(!out.contains("Every link is attested"), "{out}");
}

#[test]
fn a2_an_unsigned_review_note_reads_as_a_claim() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    let note = out
        .lines()
        .find(|l| l.contains("Retry bei 5xx fehlt"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(note.contains("◌ pd"), "{out}");
    assert!(!note.contains("(signed)"), "{out}");
    // Ein Urteil nur aus unsignierten Notizen ist aufgezeichnet, nicht
    // belegt: Der REVIEW-Kopf trägt den Claim-Marker, nie das grüne ✓.
    assert!(out.contains("◌ REVIEW"), "{out}");
    assert!(!out.contains("✓ REVIEW"), "{out}");
    let buffer = buffer_sized(&mut app, 160, 40);
    let (glyph, color) = marker_of(&buffer, "REVIEW").expect("REVIEW link");
    assert_eq!(glyph, "◌");
    assert_ne!(color, theme::OK);
}
