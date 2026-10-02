//! Die Liste: eine Tabelle mit benannten Spalten — Zeit, Überschrift, Akteur,
//! Umfang, Seal, Verdict. Spaltenköpfe und Rahmen, damit ein Außenstehender
//! die Beweisspalten ohne Legende liest (das llmfit-Vorbild der Demo).
//!
//! Die Liste zeichnet in jede Breite: Sie wählt ihre Spalten nach dem
//! `Rect`, das sie bekommt ([`Columns`]) — als Spalte neben der Vorschau
//! genauso wie über den ganzen Bildschirm.

use minds_reader::model::{CardState, SessionCard};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::theme;
use crate::view::{clip, offset, when};

/// Die schmalste Breite, in der die kompakte Stufe alle ihre Spalten zeigt
/// — die Untergrenze für die Listenspalte neben der Vorschau.
pub const COMPACT_WIDTH: u16 = 64;
/// Ab hier stehen alle Beweisspalten (Seal **und** Verdict) nebeneinander.
pub const FULL_WIDTH: u16 = 97;
/// Ab hier kommt der Umfang (Dateien, Tokens) dazu.
pub const WIDE_WIDTH: u16 = 120;

const TIME_W: u16 = 13;
const AGENT_W: u16 = 22;
const AGENT_COMPACT_W: u16 = 12;
const SIZE_W: u16 = 20;
const SEAL_W: u16 = 18;
const VERDICT_W: u16 = 14;
/// Rahmen links und rechts.
const BORDER_W: u16 = 2;
/// Der Zeilen-Glyph vor der Überschrift (`● `).
const GLYPH_W: u16 = 2;

/// Welche Spalten die Breite trägt. Was zuerst weicht, ist was der
/// Graph-Kopf daneben ohnehin sagt: erst der Umfang, dann das Review-
/// Verdict; die Seal-Spalte — der Manipulationsbefund — bleibt immer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Columns {
    /// Zeit · Session · Agent (gekürzt) · Seal.
    Compact,
    /// Zeit · Session · Agent · Seal · Verdict.
    Full,
    /// Dazu der Umfang.
    Wide,
}

impl Columns {
    fn for_width(width: u16) -> Self {
        if width >= WIDE_WIDTH {
            Self::Wide
        } else if width >= FULL_WIDTH {
            Self::Full
        } else {
            Self::Compact
        }
    }

    fn agent_w(self) -> u16 {
        match self {
            Self::Compact => AGENT_COMPACT_W,
            Self::Full | Self::Wide => AGENT_W,
        }
    }

    fn show_size(self) -> bool {
        self == Self::Wide
    }

    fn show_verdict(self) -> bool {
        self != Self::Compact
    }

    /// Die kleinste Breite der Überschrift — darunter schnitte die Table
    /// hart, ohne „…".
    fn headline_min(self) -> u16 {
        match self {
            Self::Compact => 16,
            Self::Full | Self::Wide => 24,
        }
    }

    fn header(self) -> Vec<&'static str> {
        let mut header = vec!["TIME", "SESSION", "AGENT"];
        if self.show_size() {
            header.push("SIZE");
        }
        header.push("SEAL");
        if self.show_verdict() {
            header.push("VERDICT");
        }
        header
    }

    /// Die Breiten der Fixspalten rechts der Überschrift, in
    /// Tabellenreihenfolge.
    fn trailing(self) -> Vec<u16> {
        let mut trailing = vec![self.agent_w()];
        if self.show_size() {
            trailing.push(SIZE_W);
        }
        trailing.push(SEAL_W);
        if self.show_verdict() {
            trailing.push(VERDICT_W);
        }
        trailing
    }

    fn widths(self) -> Vec<Constraint> {
        let mut widths = vec![
            Constraint::Length(TIME_W),
            Constraint::Min(self.headline_min()),
        ];
        widths.extend(self.trailing().into_iter().map(Constraint::Length));
        widths
    }

    /// Was die Überschrift bekommt: der Rest nach den Fixspalten — geclippt
    /// an der realen Spaltenbreite, nicht an einer Konstante.
    fn headline_w(self, table_w: u16) -> usize {
        let trailing = self.trailing();
        let fixed: u16 = TIME_W + trailing.iter().sum::<u16>();
        let spacing = trailing.len() as u16 + 1; // Spalten − 1
        table_w
            .saturating_sub(fixed + spacing + BORDER_W + GLYPH_W)
            .max(self.headline_min() - GLYPH_W) as usize
    }
}

/// Zeichnet die Liste.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    // Zurückgehaltene Sessions (Block-Seals, ADR-0011): eine eigene Zeile
    // UNTER der Tabelle — die Abwesenheit einer Session ist eine Aussage,
    // und eine Aussage, die unter dem Scroll-Fenster hängt, sieht niemand.
    let rejected = app.inspection.rejected_seals();
    let notice_h = if app.query.is_empty() && !rejected.is_empty() {
        1
    } else {
        0
    };
    let [table_area, notice_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(notice_h)]).areas(area);

    let cards = app.visible_cards();
    if cards.is_empty() {
        let text = if app.cards.is_empty() {
            "No sessions captured yet.\n\nminds enable installs the hooks; after the next commit the first session appears here."
        } else {
            "No match for the search.\n\nEsc clears the search."
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(theme::dim())
                .block(sessions_block()),
            table_area,
        );
        draw_notice(frame, notice_area, rejected.len());
        return;
    }

    // Rahmen (2) + Kopfzeile (1) gehen von der Höhe ab.
    let height = (table_area.height as usize).saturating_sub(3).max(1);
    let first = offset(app.cursor, cards.len(), height);
    let columns = Columns::for_width(table_area.width);
    let headline_w = columns.headline_w(table_area.width);

    let header = Row::new(
        columns
            .header()
            .into_iter()
            .map(|h| Cell::from(Span::styled(h, theme::title()))),
    );

    let rows = cards
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, card)| {
            let mut row = row_of(card, columns, headline_w);
            if i == app.cursor {
                row = row.style(theme::cursor());
            }
            row
        });

    frame.render_widget(
        Table::new(rows, columns.widths())
            .header(header)
            .column_spacing(1)
            .block(sessions_block()),
        table_area,
    );
    draw_notice(frame, notice_area, rejected.len());
}

fn sessions_block() -> Block<'static> {
    Block::bordered()
        .title(" SESSIONS ")
        .title_style(theme::title())
}

/// Eine Karte als Tabellenzeile. Degradierte Zeilen sind gedimmt und tragen
/// ihren Zustand in der SEAL-Spalte — das Verdict bleibt leer, wo keines ist.
fn row_of<'a>(card: &'a SessionCard, columns: Columns, headline_w: usize) -> Row<'a> {
    let degraded = card.is_degraded();
    let base = if degraded {
        theme::dim()
    } else {
        Style::default()
    };

    let session = Line::from(vec![
        Span::styled(
            if degraded { "⌦ " } else { "● " },
            if degraded {
                theme::dim()
            } else {
                theme::lane(0)
            },
        ),
        Span::styled(
            clip(&card.summary.headline, headline_w),
            base.patch(theme::title()),
        ),
    ]);

    let mut cells = vec![
        Cell::from(Span::styled(when(card.started_at.as_deref()), base)),
        Cell::from(session),
        // Je Agent eine feste Farbe — das Wort steht daneben. Eine
        // degradierte Zeile bleibt ganz grau: Ihr Akteur ist „—", kein Agent.
        Cell::from(Span::styled(
            clip(&card.summary.actor, columns.agent_w() as usize),
            if degraded {
                base
            } else {
                base.fg(theme::agent_color(&card.summary.actor))
            },
        )),
    ];
    if columns.show_size() {
        cells.push(Cell::from(Span::styled(
            format!(
                "{} D · {}/{} T",
                card.summary.files, card.summary.input_tokens, card.summary.output_tokens
            ),
            base,
        )));
    }
    if degraded {
        cells.push(Cell::from(Span::styled(state_word(card), theme::dim())));
        if columns.show_verdict() {
            cells.push(Cell::from(""));
        }
    } else {
        // Drei Beweiszustände (ADR-0011), jetzt mit Spaltenkopf: das
        // Seal-Verdikt als WORT, der beste Kanten-Beleg als Glyph daneben
        // (das Wort erklärt die Fußzeile für die fokussierte Karte), das
        // Review-Verdict als eigene Spalte.
        let (s_glyph, s_word, s_style) = theme::provenance(&card.provenance);
        let (ev_glyph, _, ev_style) = theme::evidence(card.evidence);
        cells.push(Cell::from(Line::from(vec![
            Span::styled(format!("{s_glyph} {s_word}"), s_style),
            Span::raw("  "),
            Span::styled(ev_glyph, ev_style),
        ])));
        if columns.show_verdict() {
            let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
            cells.push(Cell::from(Span::styled(
                format!("{v_glyph} {v_word}"),
                v_style,
            )));
        }
    }
    Row::new(cells)
}

fn draw_notice(frame: &mut Frame, area: Rect, rejected: usize) {
    if area.height == 0 || rejected == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⛔ ", Style::default().fg(theme::DELETE)),
            Span::styled(
                format!(
                    "{rejected} session(s) withheld (redaction) — coverage sealed, \
                     details: minds fsck"
                ),
                Style::default().fg(theme::REVIEW),
            ),
        ])),
        area,
    );
}

fn state_word(card: &SessionCard) -> &'static str {
    match card.state {
        CardState::Ok => "",
        CardState::Forgotten { .. } => "⌦ forgotten",
        CardState::Unreadable { .. } => "? unreadable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Stufen-Schwellen sind lastentragend (`SPLIT_MIN_WIDTH`, die
    /// Listenspalte): An der kompakten und der vollen Schwelle geht die
    /// Tabelle auf den Punkt auf — die Überschrift bekommt genau ihr
    /// Minimum; die breite Schwelle (120, der historische Wert) lässt ihr
    /// etwas Luft, nie weniger.
    #[test]
    fn every_tier_fits_at_its_threshold() {
        for (width, tier, exact) in [
            (COMPACT_WIDTH, Columns::Compact, true),
            (FULL_WIDTH, Columns::Full, true),
            (WIDE_WIDTH, Columns::Wide, false),
        ] {
            let columns = Columns::for_width(width);
            assert_eq!(columns, tier);
            let min = (tier.headline_min() - GLYPH_W) as usize;
            let got = columns.headline_w(width);
            if exact {
                assert_eq!(got, min, "{tier:?} at {width}");
            } else {
                assert!(got >= min, "{tier:?} at {width}: {got} < {min}");
            }
            assert_eq!(columns.headline_w(width + 1), got + 1, "{tier:?}");
        }
        assert_eq!(Columns::for_width(FULL_WIDTH - 1), Columns::Compact);
        assert_eq!(Columns::for_width(WIDE_WIDTH - 1), Columns::Full);
        assert_eq!(Columns::for_width(0), Columns::Compact);
    }
}
