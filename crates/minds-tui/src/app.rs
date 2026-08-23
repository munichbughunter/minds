//! Der Zustand der Oberfläche und seine Übergänge — `reduce` ist reine
//! Zustandsänderung und ohne Terminal prüfbar; nur `run` hält die Schleife.
//!
//! Drei Projektionen desselben Modells liegen als Stapel übereinander:
//! unten die Activity-Liste, darüber Graphen und Herkunftsketten, so tief,
//! wie der Nutzer hineingeht. `Esc` nimmt die oberste Ebene weg; auf der
//! Liste löscht es erst die Suche und beendet dann.

use std::time::Duration;

use crossterm::event::{self, Event};
use minds_core::SessionId;
use minds_git::Repo;
use minds_reader::Inspection;
use minds_reader::graph::{NodeKind, SessionGraph};
use minds_reader::model::{LinkEvidence, SessionCard, WhyChain, WhyStep};

use crate::filter;
use crate::input::{self, Action};
use crate::layout::{self, Row, Zoom};
use crate::term::Guard;
use crate::view;

/// Wie lange auf eine Taste gewartet wird, bevor neu gezeichnet wird
/// (Größenänderung des Terminals).
const TICK: Duration = Duration::from_millis(250);

/// Eine Ebene über der Liste.
#[derive(Debug, Clone)]
pub enum View {
    /// Der Graph einer Session.
    Graph {
        /// Die Session.
        id: SessionId,
        /// Der Graph.
        graph: SessionGraph,
        /// Die gezeichneten Zeilen (Baum oder Zeitleiste).
        rows: Vec<Row>,
        /// Die Cursorzeile.
        cursor: usize,
        /// Zeitleiste statt Baum.
        timeline: bool,
    },
    /// Eine Herkunftskette.
    Why {
        /// Die Kette.
        chain: WhyChain,
        /// Das Glied unter dem Cursor.
        cursor: usize,
        /// Der geöffnete Evidence-Inspector, je Kante eine Erklärung.
        inspector: Option<Vec<LinkEvidence>>,
    },
}

/// Der Zustand.
pub struct App<'a> {
    /// Das Lese-Modell.
    pub inspection: Inspection,
    /// Für die zwei Fragen, die nach dem Start noch Git brauchen.
    pub repo: &'a Repo,
    /// Alle Karten.
    pub cards: Vec<SessionCard>,
    /// Die Karten, die die Suche durchlässt — Indizes in `cards`.
    pub visible: Vec<usize>,
    /// Die Cursorzeile in `visible`.
    pub cursor: usize,
    /// Die Suche.
    pub query: String,
    /// Ob gerade in die Suche getippt wird.
    pub searching: bool,
    /// Die Detailstufe.
    pub zoom: Zoom,
    /// Die Ebenen über der Liste.
    pub views: Vec<View>,
    /// Ob die Hilfe liegt.
    pub help: bool,
    /// Ob die Schleife enden soll.
    pub quit: bool,
    /// Zeilen je Seite — setzt die Zeichenroutine.
    pub page: usize,
}

impl<'a> App<'a> {
    /// Baut den Zustand; `query` seedet die Suche.
    pub fn new(inspection: Inspection, repo: &'a Repo, query: Option<String>) -> Self {
        let cards = inspection.cards();
        let mut app = Self {
            inspection,
            repo,
            cards,
            visible: Vec::new(),
            cursor: 0,
            query: query.unwrap_or_default(),
            searching: false,
            zoom: Zoom::Normal,
            views: Vec::new(),
            help: false,
            quit: false,
            page: 20,
        };
        app.refilter();
        app
    }

    /// Die Karten, die gerade sichtbar sind.
    pub fn visible_cards(&self) -> Vec<&SessionCard> {
        self.visible.iter().map(|i| &self.cards[*i]).collect()
    }

    /// Die Karte unter dem Cursor.
    pub fn selected(&self) -> Option<&SessionCard> {
        self.visible.get(self.cursor).map(|i| &self.cards[*i])
    }

    /// Die oberste Ebene.
    pub fn top(&self) -> Option<&View> {
        self.views.last()
    }

    /// Öffnet die Herkunftskette einer Zeile — der Einstieg über
    /// `minds inspect <datei>:<zeile>`.
    pub fn open_why_line(&mut self, path: &str, line: u32) -> minds_reader::Result<()> {
        let chain = self.inspection.why_line(self.repo, path, line)?;
        self.push_why(chain);
        Ok(())
    }

    fn refilter(&mut self) {
        let keep = self.selected().map(|c| c.id);
        let terms = filter::terms(&self.query);
        self.visible = self
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| filter::matches(card, self.inspection.index(), &terms))
            .map(|(i, _)| i)
            .collect();
        self.cursor = keep
            .and_then(|id| self.visible.iter().position(|i| self.cards[*i].id == id))
            .unwrap_or(0);
    }

    fn push_graph(&mut self, id: SessionId) {
        let Some(graph) = self.inspection.graph(id) else {
            return;
        };
        let rows = layout::rows(&graph, self.zoom);
        self.views.push(View::Graph {
            id,
            graph,
            rows,
            cursor: 0,
            timeline: false,
        });
    }

    fn push_why(&mut self, chain: WhyChain) {
        self.views.push(View::Why {
            chain,
            cursor: 0,
            inspector: None,
        });
    }

    fn relayout(&mut self) {
        let zoom = self.zoom;
        for view in &mut self.views {
            if let View::Graph {
                graph,
                rows,
                cursor,
                timeline,
                ..
            } = view
            {
                *rows = if *timeline {
                    layout::timeline(graph, zoom)
                } else {
                    layout::rows(graph, zoom)
                };
                *cursor = (*cursor).min(rows.len().saturating_sub(1));
            }
        }
    }

    /// Wendet eine Aktion an.
    pub fn reduce(&mut self, action: Action) {
        if action == Action::Quit {
            self.quit = true;
            return;
        }
        if self.help {
            if matches!(action, Action::Help | Action::Back | Action::Enter) {
                self.help = false;
            }
            return;
        }
        if self.searching {
            match action {
                Action::SearchInput(c) => {
                    self.query.push(c);
                    self.refilter();
                }
                Action::SearchBackspace => {
                    self.query.pop();
                    self.refilter();
                }
                Action::SearchCommit => self.searching = false,
                Action::Back => {
                    self.query.clear();
                    self.searching = false;
                    self.refilter();
                }
                Action::Up => self.cursor = self.cursor.saturating_sub(1),
                Action::Down => self.move_list(1),
                _ => {}
            }
            return;
        }
        if let Action::Zoom(d) = action {
            self.zoom = self.zoom.from_digit(d);
            self.relayout();
            return;
        }
        if action == Action::Help {
            self.help = true;
            return;
        }
        let page = self.page.max(1);
        match self.views.pop() {
            None => self.reduce_activity(action, page),
            Some(View::Graph {
                id,
                graph,
                rows,
                mut cursor,
                mut timeline,
            }) => {
                let mut keep = true;
                match action {
                    Action::Up => cursor = cursor.saturating_sub(1),
                    Action::Down => cursor = (cursor + 1).min(rows.len().saturating_sub(1)),
                    Action::PageUp => cursor = cursor.saturating_sub(page),
                    Action::PageDown => cursor = (cursor + page).min(rows.len().saturating_sub(1)),
                    Action::Home => cursor = 0,
                    Action::End => cursor = rows.len().saturating_sub(1),
                    Action::Back => keep = false,
                    Action::ToggleTimeline => timeline = !timeline,
                    Action::Why => {
                        self.views.push(View::Graph {
                            id,
                            graph,
                            rows,
                            cursor,
                            timeline,
                        });
                        if let Some(chain) = self.inspection.why_session(id) {
                            self.push_why(chain);
                        }
                        return;
                    }
                    Action::Enter => {
                        let target = rows.get(cursor).map(|r| r.kind.clone());
                        self.views.push(View::Graph {
                            id,
                            graph,
                            rows,
                            cursor,
                            timeline,
                        });
                        match target {
                            Some(NodeKind::Subagent(child)) => self.push_graph(child),
                            Some(
                                NodeKind::Change(_) | NodeKind::Commit(_) | NodeKind::Review(_),
                            ) => {
                                if let Some(chain) = self.inspection.why_session(id) {
                                    self.push_why(chain);
                                }
                            }
                            _ => {}
                        }
                        return;
                    }
                    _ => {}
                }
                if keep {
                    let rows = if timeline {
                        layout::timeline(&graph, self.zoom)
                    } else {
                        layout::rows(&graph, self.zoom)
                    };
                    let cursor = cursor.min(rows.len().saturating_sub(1));
                    self.views.push(View::Graph {
                        id,
                        graph,
                        rows,
                        cursor,
                        timeline,
                    });
                }
            }
            Some(View::Why {
                chain,
                mut cursor,
                mut inspector,
            }) => {
                let last = chain.steps.len().saturating_sub(1);
                let mut keep = true;
                // Der Inspector folgt dem Fokus: Bewegung schließt ihn und
                // öffnet ihn nur auf dem Evidence-Glied neu; Esc schließt
                // ihn, ohne dass er sofort wiederkommt.
                let mut follow = true;
                match action {
                    Action::Up => {
                        inspector = None;
                        cursor = cursor.saturating_sub(1);
                    }
                    Action::Down => {
                        inspector = None;
                        cursor = (cursor + 1).min(last);
                    }
                    Action::Home | Action::PageUp => {
                        inspector = None;
                        cursor = 0;
                    }
                    Action::End | Action::PageDown => {
                        inspector = None;
                        cursor = last;
                    }
                    Action::Back => {
                        follow = false;
                        if inspector.is_some() {
                            inspector = None;
                        } else {
                            keep = false;
                        }
                    }
                    Action::Enter => match chain.steps.get(cursor) {
                        Some(WhyStep::Evidence { links }) => {
                            inspector = Some(self.inspection.explain_links(self.repo, links));
                        }
                        Some(WhyStep::Sessions { cards }) => {
                            if let Some(card) = cards.first() {
                                let id = card.id;
                                self.views.push(View::Why {
                                    chain,
                                    cursor,
                                    inspector,
                                });
                                self.push_graph(id);
                                return;
                            }
                        }
                        Some(WhyStep::Commit {
                            id: Some(commit), ..
                        }) => {
                            let commit = *commit;
                            self.views.push(View::Why {
                                chain,
                                cursor,
                                inspector,
                            });
                            let chain = self.inspection.why_commit(commit);
                            self.push_why(chain);
                            return;
                        }
                        _ => {}
                    },
                    _ => {}
                }
                if keep {
                    // Die Erklärung gehört zum Fokus, nicht zum Enter: Steht
                    // der Cursor auf dem Evidence-Glied, wird jede Kante
                    // erklärt — „?" soll nie nur „irgendwie unsicher" heißen.
                    if follow
                        && inspector.is_none()
                        && let Some(WhyStep::Evidence { links }) = chain.steps.get(cursor)
                    {
                        inspector = Some(self.inspection.explain_links(self.repo, links));
                    }
                    self.views.push(View::Why {
                        chain,
                        cursor,
                        inspector,
                    });
                }
            }
        }
    }

    fn move_list(&mut self, by: usize) {
        self.cursor = (self.cursor + by).min(self.visible.len().saturating_sub(1));
    }

    fn reduce_activity(&mut self, action: Action, page: usize) {
        match action {
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::Down => self.move_list(1),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(page),
            Action::PageDown => self.move_list(page),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = self.visible.len().saturating_sub(1),
            Action::Enter => {
                if let Some(card) = self.selected().filter(|c| !c.is_degraded()) {
                    self.push_graph(card.id);
                }
            }
            Action::Why => {
                if let Some(card) = self.selected().filter(|c| !c.is_degraded())
                    && let Some(chain) = self.inspection.why_session(card.id)
                {
                    self.push_why(chain);
                }
            }
            Action::SearchStart => self.searching = true,
            Action::Back => {
                if self.query.is_empty() {
                    self.quit = true;
                } else {
                    self.query.clear();
                    self.refilter();
                }
            }
            _ => {}
        }
    }

    /// Die Schleife: zeichnen, Taste lesen, anwenden — bis `quit`.
    pub fn run(mut self) -> std::io::Result<()> {
        let (_guard, mut terminal) = Guard::take()?;
        while !self.quit {
            terminal.draw(|frame| view::draw(frame, &mut self))?;
            if event::poll(TICK)?
                && let Event::Key(key) = event::read()?
                && key.kind != event::KeyEventKind::Release
            {
                let action = input::map(key, self.searching);
                self.reduce(action);
            }
        }
        Ok(())
    }
}
