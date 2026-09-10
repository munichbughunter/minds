//! Die Liste: eine Tabelle mit benannten Spalten — Zeit, Überschrift, Akteur,
//! Umfang, Seal, Verdict. Spaltenköpfe und Rahmen, damit ein Außenstehender
//! die Beweisspalten ohne Legende liest (das llmfit-Vorbild der Demo).

use minds_reader::model::{CardState, SessionCard};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::theme;
use crate::view::{clip, offset, when};

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
    let show_size = table_area.width >= 120;

    let mut header = vec!["TIME", "SESSION", "AGENT"];
    if show_size {
        header.push("SIZE");
    }
    header.extend(["SEAL", "VERDICT"]);
    let header = Row::new(
        header
            .into_iter()
            .map(|h| Cell::from(Span::styled(h, theme::title()))),
    );

    let mut widths = vec![
        Constraint::Length(13),
        Constraint::Min(24),
        Constraint::Length(22),
    ];
    if show_size {
        widths.push(Constraint::Length(20));
    }
    widths.extend([Constraint::Length(18), Constraint::Length(14)]);

    // Die Überschrift bekommt, was nach den Fixspalten übrig ist — geclippt
    // an der realen Spaltenbreite, nicht an einer Konstante (sonst schnitte
    // die Table auf schmalen Terminals hart, ohne „…").
    let fixed: usize = 13 + 22 + 18 + 14 + if show_size { 20 } else { 0 };
    let columns = if show_size { 6 } else { 5 };
    let headline_w = (table_area.width as usize)
        .saturating_sub(fixed + (columns - 1) + 2 /* Rahmen */ + 2 /* Glyph */)
        .max(24);

    let rows = cards
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, card)| {
            let mut row = row_of(card, show_size, headline_w);
            if i == app.cursor {
                row = row.style(theme::cursor());
            }
            row
        });

    frame.render_widget(
        Table::new(rows, widths)
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
fn row_of<'a>(card: &'a SessionCard, show_size: bool, headline_w: usize) -> Row<'a> {
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
        Cell::from(Span::styled(
            clip(&card.summary.actor, 22),
            base.fg(theme::AGENT),
        )),
    ];
    if show_size {
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
        cells.push(Cell::from(""));
    } else {
        // Drei Beweiszustände (ADR-0011), jetzt mit Spaltenkopf: das
        // Seal-Verdikt als WORT, der beste Kanten-Beleg als Glyph daneben
        // (das Wort erklärt die Fußzeile für die fokussierte Karte), das
        // Review-Verdict als eigene Spalte.
        let (s_glyph, s_word, s_style) = theme::provenance(&card.provenance);
        let (ev_glyph, _, ev_style) = theme::evidence(card.evidence);
        let (v_glyph, v_word, v_style) = theme::verdict(card.review.verdict);
        cells.push(Cell::from(Line::from(vec![
            Span::styled(format!("{s_glyph} {s_word}"), s_style),
            Span::raw("  "),
            Span::styled(ev_glyph, ev_style),
        ])));
        cells.push(Cell::from(Span::styled(
            format!("{v_glyph} {v_word}"),
            v_style,
        )));
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
