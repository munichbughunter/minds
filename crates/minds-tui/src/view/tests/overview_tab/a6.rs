//! A6 · Überläufe: Kopf, Fußzeile, Hilfe, Why-Höhe — nach der
//! Test-Konvention in 80×24, 120×30 und 160×40, für jeden Tab.

use super::*;

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 30), (160, 40)];

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal
        .backend()
        .to_string()
        .lines()
        .map(|l| l.trim_matches('"').to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Was den Vertrauenszustand des fokussierten Objekts trägt, je Tab — der
/// Changes- und der Requirements-Tab fokussieren eine Datei bzw. einen
/// Anker, keinen Gegenstand mit Seal-Zustand. (Die Urteilswörter des
/// früheren Verify-Tabs stehen seit C12.2 in EVIDENCE der Übersicht.)
fn trust_words(tab: Tab) -> &'static [&'static str] {
    match tab {
        Tab::Overview => &["sealed", "unsealed", "incomplete", "TAMPERED", "legacy"],
        Tab::Sessions => &["sealed", "legacy", "TAMPERED"],
        Tab::Changes | Tab::Requirements => &[],
    }
}

#[test]
fn a6_every_tab_fits_in_every_size() {
    for (w, h) in SIZES {
        for (i, tab) in Tab::ALL.iter().enumerate() {
            let (_dir, repo, _, inspection) = history();
            let mut app = opened(&repo, inspection);
            app.reduce(Action::TabTo(i as u8));
            assert_eq!(app.tab, *tab);
            let out = render_sized(&mut app, w, h);
            let lines: Vec<&str> = out.lines().collect();
            // Die Tab-Leiste vollständig: jede Taste, jedes (Kurz-)Label.
            for (k, t) in Tab::ALL.iter().enumerate() {
                let key = format!("F{}", k + 1);
                assert!(lines[0].contains(&key), "{w}x{h} {tab:?}: {key}\n{out}");
                assert!(
                    lines[0].contains(t.short()) || lines[0].contains(t.title()),
                    "{w}x{h} {tab:?}: {}\n{out}",
                    t.title()
                );
            }
            // `? help` und der Ausgang, rechts fixiert, in der letzten Zeile.
            let last = lines[lines.len() - 1].trim_end();
            assert!(last.ends_with("? help  q quit"), "{w}x{h} {tab:?}\n{out}");
            // Der Vertrauenszustand des fokussierten Objekts.
            let words = trust_words(*tab);
            if !words.is_empty() {
                assert!(
                    words.iter().any(|word| out.contains(word)),
                    "{w}x{h} {tab:?}: no trust state\n{out}"
                );
            }
            // Kopf und Fußzeile: nichts hart abgeschnitten — eine volle
            // Zeile endet mit „…" oder mit dem fixierten Ausgang.
            for line in [lines[0], lines[1], lines[lines.len() - 2]] {
                if line.chars().count() as u16 >= w && !line.ends_with(' ') {
                    assert!(
                        line.ends_with('…') || line.ends_with("q quit"),
                        "{w}x{h} {tab:?}: cut without …: {line:?}"
                    );
                }
            }
        }
    }
}

/// Unter 100 Spalten Kurzlabels; Repo und Branch weichen zuerst, nie die Tabs.
#[test]
fn a6_narrow_headers_use_short_labels() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    let out = render_sized(&mut app, 80, 24);
    let head = out.lines().next().unwrap();
    for label in ["F1 Ovw", "F2 Chg", "F3 Ses", "F4 Req"] {
        assert!(head.contains(label), "{head}");
    }
    let out = render_sized(&mut app, 160, 40);
    assert!(out.lines().next().unwrap().contains("F1 Overview"), "{out}");
}

/// Die Hilfe ist scrollbar und nennt den aktuellen Tab zuerst.
#[test]
fn a6_the_help_scrolls_and_starts_with_the_current_tab() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::TabTo(1)); // Changes
    app.reduce(Action::Help);
    let out = render_sized(&mut app, 80, 24);
    let changes = out.find("Changes:").unwrap_or_else(|| panic!("{out}"));
    if let Some(general) = out.find("Enter / l") {
        assert!(changes < general, "current tab first\n{out}");
    }
    // Bei 24 Zeilen passt nicht alles — die Legende ist erreichbar.
    assert!(!out.contains("Kinds:"), "{out}");
    // Scrollen erreicht die Legende (die Hilfe ist länger als sie).
    let mut reached = false;
    for _ in 0..120 {
        app.reduce(Action::Down);
        if render_sized(&mut app, 80, 24).contains("Kinds:") {
            reached = true;
            break;
        }
    }
    assert!(app.help, "scrolling keeps the help open");
    assert!(reached, "the legend is reachable by scrolling");
    app.reduce(Action::Home);
    let out = render_sized(&mut app, 80, 24);
    assert!(out.contains("Changes:"), "{out}");
    app.reduce(Action::Back);
    assert!(!app.help);
}

/// Beim Tippen einer Suche sind `?` und `q` Zeichen der Suche: Die Fußzeile
/// verspricht sie dort nicht, sondern die Suchaktionen.
#[test]
fn a6_the_search_footer_pins_the_search_keys() {
    let (_dir, repo) = super::super::repo();
    let mut app = App::new(super::super::filled(), &repo, None);
    app.reduce(Action::SearchStart);
    for (w, h) in SIZES {
        let out = render_sized(&mut app, w, h);
        let last = out.lines().last().unwrap().trim_end();
        assert!(!last.contains("q quit"), "{w}x{h}\n{out}");
        assert!(!last.contains("? help"), "{w}x{h}\n{out}");
        assert!(last.ends_with("Enter apply  Esc clear"), "{w}x{h}\n{out}");
    }
}

/// Nach `End` scrollt ↑ sofort zurück — der Wert bleibt im Möglichen.
#[test]
fn a6_the_help_scroll_stays_in_range() {
    let (_dir, repo) = super::super::repo();
    let mut app = App::new(super::super::filled(), &repo, None);
    app.reduce(Action::Help);
    app.reduce(Action::End);
    let end = render_sized(&mut app, 80, 24);
    app.reduce(Action::Up);
    let up = render_sized(&mut app, 80, 24);
    assert_ne!(end, up, "↑ after End moves at once");
}
