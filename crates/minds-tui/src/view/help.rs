//! Die Hilfe — ein Overlay über allem.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::Tab;

use crate::theme;

const TEXT: &str = "\
j / k  ↑ / ↓      one line
PgUp / PgDn        one page
g / G              start / end
Enter / l          open, descend
Esc / h            back: close, clear the search, return — never quits

/                  filter in every tab: sessions, commits, files, anchors (AND)
w                  why — the provenance chain (on any session, commit or line)
e                  evidence of the session — verdict, coverage, epochs, signature
v / d / i          verify · diff · intent of the commit or session in focus
t                  graph ↔ timeline
z / Z              zoom: finer / coarser
r                  reload now (it also reloads by itself when the store changes)
1 … 5 / Tab / F1   switch tab: Overview · Sessions · Verify · Changes · Intent
?                  this help (↑↓ PgUp PgDn scroll it)
q / Ctrl-C         quit";

/// Die Tasten eines Tabs — der Abschnitt des aktuellen steht oben (A6).
fn section(tab: Tab) -> &'static str {
    match tab {
        Tab::Overview => {
            "Overview: ↑↓ commit · Enter its session(s) / graph · w why chain · Esc back (sessions)\n          ◉CC agent session · PD author initials · @branch #tag · ⚑ intent · ◉N sessions"
        }
        Tab::Sessions => {
            "Sessions: ↑↓ session · Enter graph · w why · e evidence · t timeline · / search"
        }
        Tab::Verify => "Verify:   ↑↓ row · Enter open (session, diff) · [ ] commit",
        Tab::Changes => {
            "Changes:  Enter diff · ] [ hunk (commit in the file list) · n N next unexplained\n          s unified ↔ split · c why column · w why chain of the line · Esc back"
        }
        Tab::Intent => {
            "Intent:   ↑↓ anchor · PgUp PgDn scroll · Enter graph of a session naming it\n          in why: Enter on the anchor"
        }
    }
}

/// Der ganze Inhalt: der aktuelle Tab, die allgemeinen Tasten, die übrigen
/// Tabs, die Legende.
fn content(tab: Tab, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = section(tab).lines().map(str::to_string).collect();
    lines.push(String::new());
    lines.extend(TEXT.lines().map(str::to_string));
    lines.push(String::new());
    lines.extend(legend(width));
    lines.push(String::new());
    for other in Tab::ALL.iter().filter(|t| **t != tab) {
        lines.extend(section(*other).lines().map(str::to_string));
    }
    // Das Glossar (C1): dieselben Sätze, die die Fußzeile im Kontext zeigt.
    lines.push(String::new());
    lines.push("Glossary".to_string());
    for term in crate::copy::GLOSSARY {
        lines.extend(wrap(term.sentence, width));
    }
    lines
}

/// Bricht einen Satz an Wortgrenzen auf `width` um (Folgezeilen eingerückt).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
            line.push_str("  ");
        }
        if !line.is_empty() && !line.ends_with("  ") {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out
}

/// Die Legende, aus dem Glyph-Register erzeugt (A1): erst die Zustände
/// (epistemisch), dann die Arten (strukturell), auf `width` umbrochen.
pub fn legend(width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (label, class) in [
        ("State:  ", theme::GlyphClass::Epistemic),
        ("Kinds:  ", theme::GlyphClass::Structural),
    ] {
        let mut line = label.to_string();
        for entry in theme::REGISTER.iter().filter(|e| e.class == class) {
            let item = format!("{} {}", entry.symbol, entry.word);
            let fresh = line.chars().count() == label.len();
            let sep = if fresh { "" } else { "   " };
            if !fresh && line.chars().count() + sep.len() + item.chars().count() > width {
                lines.push(line);
                line = " ".repeat(label.len());
                line.push_str(&item);
            } else {
                line.push_str(sep);
                line.push_str(&item);
            }
        }
        lines.push(line);
    }
    lines
}

/// Zeichnet die Hilfe mittig — so hoch wie ihr Inhalt, höchstens das
/// Terminal; was nicht passt, erreicht man mit ↑↓ / PgUp / PgDn. Gibt den
/// tatsächlich gezeigten Scroll-Wert zurück — der Zustand bleibt so im
/// Möglichen, ↑ nach `End` wirkt sofort.
pub fn draw(frame: &mut Frame, area: Rect, tab: Tab, scroll: u16) -> u16 {
    let width = 84.min(area.width);
    let lines = content(tab, width.saturating_sub(2) as usize);
    let height = (lines.len() as u16 + 2).min(area.height);
    let inner = height.saturating_sub(2);
    let max = (lines.len() as u16).saturating_sub(inner);
    let scroll = scroll.min(max);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let title = if max > 0 {
        format!(" Help · {}/{} · ↑↓ scroll ", scroll + inner, lines.len())
    } else {
        " Help ".to_string()
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .scroll((scroll, 0))
            .block(Block::bordered().title(title).title_style(theme::title())),
        rect,
    );
    scroll
}
