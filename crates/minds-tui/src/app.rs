//! Der Zustand der Oberfläche und seine Übergänge — `reduce` ist reine
//! Zustandsänderung und ohne Terminal prüfbar; nur `run` hält die Schleife.
//!
//! Drei Projektionen desselben Modells liegen als Stapel übereinander:
//! unten die Activity-Liste, darüber Graphen und Herkunftsketten, so tief,
//! wie der Nutzer hineingeht. `Esc` nimmt die oberste Ebene weg; auf der
//! Liste löscht es erst die Suche und beendet dann.
//!
//! Neu laden ersetzt das Modell, nicht den Ort: Cursor (über die
//! Session-Id), Suche und Stapel bleiben, jede Ebene wird aus ihrem
//! Ursprung neu gerechnet. Was es nicht mehr gibt, fällt samt allem
//! darüber weg.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event};
use minds_core::SessionId;
use minds_git::{CommitId, Repo};
use minds_reader::Inspection;
use minds_reader::artifact::CommitArtifact;
use minds_reader::graph::{NodeKind, SessionGraph};
use minds_reader::model::{EvidenceReport, LinkEvidence, SessionCard, WhyChain, WhyStep};

use crate::filter;
use crate::input::{self, Action};
use crate::layout::{self, Row, Zoom};
use crate::term::Guard;
use crate::view;
use crate::{Source, Stamp};

/// Wie lange auf eine Taste gewartet wird, bevor neu gezeichnet wird
/// (Größenänderung des Terminals).
const TICK: Duration = Duration::from_millis(250);

/// Wie oft die Quelle nach ihrem Fingerabdruck gefragt wird.
const CHECK: Duration = Duration::from_secs(1);

/// Wie oft `r` höchstens lädt, auch wenn die Taste gehalten wird.
const REPEAT: Duration = Duration::from_millis(250);

/// Woraus eine Herkunftskette entstand — damit Neuladen sie aus derselben
/// Frage neu rechnen kann.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyOrigin {
    /// Die Kette einer Session.
    Session(SessionId),
    /// Die Kette eines Commits.
    Commit(CommitId),
    /// Die Kette einer Zeile.
    Line {
        /// Der Pfad.
        path: String,
        /// Die Zeile, 1-basiert.
        line: u32,
    },
}

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
        /// Woraus die Kette entstand.
        origin: WhyOrigin,
        /// Die Kette.
        chain: WhyChain,
        /// Das Glied unter dem Cursor.
        cursor: usize,
        /// Die Kante unter dem Cursor, wenn das Glied das Evidence-Glied
        /// ist: ↑/↓ wandern erst über die Kanten, bevor sie das Glied
        /// verlassen (#132). Auf allen anderen Gliedern ist das `0`.
        edge: usize,
        /// Der geöffnete Evidence-Inspector — er folgt der fokussierten
        /// Kante und erklärt genau sie, nicht alle.
        inspector: Option<Vec<LinkEvidence>>,
    },
    /// Der Evidence-Report einer Session: Verdikt, Erklärung, Kryptographie
    /// — drei Ebenen über demselben, fertig gerechneten Read-Model. Die TUI
    /// rechnet nichts nach ([`minds_reader::model::EvidenceReport`]).
    Evidence {
        /// Die Session.
        id: SessionId,
        /// Der Report; `None` heißt Legacy (vor Evidence-Chain erfasst).
        report: Option<EvidenceReport>,
        /// Beobachtete, aber nicht gedeutete Tool-Aufrufe — die
        /// Deutungs-Achse neben Integrität und Coverage.
        uninterpreted: usize,
        /// Der Abgleich der Commits, die die Session tragen (EA-03) —
        /// beim Öffnen einmal gerechnet, vom Reader.
        artifacts: Vec<CommitArtifact>,
        /// Die Sektion unter dem Cursor; das Detail folgt dem Fokus.
        cursor: usize,
    },
}

/// Die Sektionen des Evidence-Reports, in Anzeige-Reihenfolge.
pub const EVIDENCE_SECTIONS: usize = 7;

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
    /// Der Fingerabdruck des geladenen Stands, wie die Quelle ihn kennt.
    pub stamp: Option<Stamp>,
    /// Ob `r` ein Neuladen verlangt hat — `run` führt es aus.
    pub reload_requested: bool,
    /// Ob das nächste Neuladen **alles** neu rechnet, auch die Git-gestützten
    /// Teile offener Ebenen (Abgleich, Inspector, Blame). Setzt `r`; ein
    /// Neuladen von selbst rechnet sie nur, wenn sich ihre Eingaben ändern.
    pub full_next: bool,
    /// Wann zuletzt erfolgreich geladen wurde, `HH:MM:SSZ`.
    pub loaded_at: String,
    /// Warum das letzte Neuladen scheiterte — dann gilt der alte Stand.
    pub reload_error: Option<String>,
    /// Der Abdruck, bei dem das Laden zuletzt scheiterte: Auf ihn wird nicht
    /// jede Sekunde erneut geladen (sonst hinge die Schleife im Laden), erst
    /// wenn er sich wieder ändert — oder auf `r`.
    pub failed_stamp: Option<Stamp>,
    /// Ob die Quelle einen Abdruck liefert. Ohne ihn lädt nichts von selbst —
    /// die Fußzeile sagt das, statt Frische vorzutäuschen.
    pub live: bool,
    /// Was beim letzten Neuladen geschlossen wurde und warum — etwa eine
    /// Zeilen-Kette, deren Datei gerade fehlt.
    pub closed: Option<String>,
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
            stamp: None,
            reload_requested: false,
            full_next: false,
            loaded_at: clock(SystemTime::now()),
            reload_error: None,
            failed_stamp: None,
            live: true,
            closed: None,
        };
        app.refilter();
        app
    }

    /// Übernimmt einen frisch geladenen Stand — oder, wenn das Laden
    /// scheiterte, behält den alten und merkt sich den Grund (fail-soft).
    ///
    /// Cursor, Suche, Zoom und Stapel bleiben; jede Ebene wird aus ihrem
    /// Ursprung neu gerechnet. Fehlt der Ursprung im neuen Stand (eine
    /// vergessene Session), fällt die Ebene samt allem darüber weg. Innerhalb
    /// einer Ebene bleibt die **Position** des Cursors, nicht der Knoten:
    /// Kommt ein Knoten dazu, kann er auf einem Nachbarn landen; eine
    /// Zeilen-Kette folgt der Zeilennummer, nicht dem Inhalt.
    ///
    /// Die Git-gestützten Teile offener Ebenen — Abgleich der Commits,
    /// Inspector, Blame — startet ein Neuladen von selbst nur neu, wenn sich
    /// ihre Eingaben geändert haben (Commits und Claimants, fokussierte
    /// Kante, HEAD). Sonst bestimmte, wer Refs schreibt, wie oft auf dem
    /// Host `git` läuft. `r` rechnet alles neu.
    pub fn reload(
        &mut self,
        loaded: minds_reader::Result<Inspection>,
        stamp: Option<Stamp>,
        now: SystemTime,
    ) {
        let asked = std::mem::take(&mut self.full_next);
        let full = asked
            || stamp.is_none()
            || self.stamp.as_ref().map(|s| &s.head) != stamp.as_ref().map(|s| &s.head);
        let inspection = match loaded {
            Ok(inspection) => inspection,
            Err(err) => {
                // Der geladene Abdruck bleibt der alte; der gescheiterte wird
                // gemerkt, damit nicht jede Sekunde erneut geladen wird.
                // `sanitize`, nicht nur Steuerzeichen: Auch Bidi- und
                // Zeilentrenner aus Ref-Namen oder Pfaden dürfen die
                // Fußzeile nicht umordnen.
                self.reload_error = Some(minds_reader::sanitize(&err.to_string()));
                self.failed_stamp = stamp;
                // Ein verlangtes volles Neuladen bleibt verlangt, bis eines
                // gelingt.
                self.full_next = asked;
                return;
            }
        };
        let keep = self.selected().map(|card| card.id);
        let at = self.cursor;
        self.cards = inspection.cards();
        let before = std::mem::replace(&mut self.inspection, inspection);
        self.refilter_keeping(keep);
        if keep.is_some() && self.selected().map(|card| card.id) != keep {
            // Die gewählte Karte ist weg: auf ihren Nachbarn, nicht an den
            // Anfang der Liste.
            self.cursor = at.min(self.visible.len().saturating_sub(1));
        }
        let old = std::mem::take(&mut self.views);
        for view in old {
            match self.rebuild(view, &before, full) {
                Ok(view) => self.views.push(view),
                Err(why) => {
                    // Nur überschreiben, nie leeren: Den Hinweis nimmt erst
                    // eine Taste weg, nicht das nächste Neuladen.
                    self.closed = Some(minds_reader::sanitize(&why));
                    break;
                }
            }
        }
        self.stamp = stamp;
        self.loaded_at = clock(now);
        self.reload_error = None;
        self.failed_stamp = None;
    }

    /// Rechnet eine Ebene gegen das aktuelle Modell neu. `Err(grund)`, wenn
    /// ihr Ursprung darin fehlt oder sich nicht rechnen ließ — das schließt
    /// die Ebene samt allem darüber, und die Fußzeile nennt den Grund.
    ///
    /// `before` ist das Modell, aus dem die Ebene stammt; ohne `full` werden
    /// Git-gestützte Teile übernommen, solange ihre Eingaben darin dieselben
    /// sind wie jetzt.
    fn rebuild(&self, view: View, before: &Inspection, full: bool) -> Result<View, String> {
        match view {
            View::Graph {
                id,
                cursor,
                timeline,
                ..
            } => {
                let graph = self.inspection.graph(id).ok_or_else(|| gone(id))?;
                let rows = if timeline {
                    layout::timeline(&graph, self.zoom)
                } else {
                    layout::rows(&graph, self.zoom)
                };
                let cursor = cursor.min(rows.len().saturating_sub(1));
                Ok(View::Graph {
                    id,
                    graph,
                    rows,
                    cursor,
                    timeline,
                })
            }
            View::Evidence {
                id,
                cursor,
                artifacts,
                ..
            } => {
                self.inspection.card(id).ok_or_else(|| gone(id))?;
                let unchanged =
                    !full && artifact_inputs(before, id) == artifact_inputs(&self.inspection, id);
                Ok(self.evidence_view_with(id, cursor, unchanged.then_some(artifacts)))
            }
            View::Why {
                origin,
                chain: old_chain,
                cursor,
                edge,
                inspector,
            } => {
                let chain = match &origin {
                    WhyOrigin::Session(id) => {
                        self.inspection.why_session(*id).ok_or_else(|| {
                            // `cards` kennt auch degradierte Sessions, `card` nicht.
                            if self.cards.iter().any(|card| card.id == *id) {
                                no_chain(*id)
                            } else {
                                gone(*id)
                            }
                        })?
                    }
                    WhyOrigin::Commit(commit) => self.inspection.why_commit(*commit),
                    WhyOrigin::Line { path, line } => match (full, blamed(&old_chain)) {
                        // HEAD steht: Blame gäbe denselben Commit — nur der
                        // Rest der Kette wird gegen das neue Modell gerechnet.
                        (false, Some((head, Some(commit)))) => {
                            let mut chain = self.inspection.why_commit(commit);
                            chain.steps.insert(0, head);
                            chain
                        }
                        (false, Some((_, None))) => old_chain,
                        _ => self
                            .inspection
                            .why_line(self.repo, path, *line)
                            .map_err(|err| format!("why {path}:{line}: {err}"))?,
                    },
                };
                let cursor = cursor.min(chain.steps.len().saturating_sub(1));
                let links = match chain.steps.get(cursor) {
                    Some(WhyStep::Evidence { links }) => links.as_slice(),
                    _ => &[],
                };
                let edge = edge.min(links.len().saturating_sub(1));
                // Ein offener Inspector erklärt nach dem Neuladen dieselbe
                // fokussierte Kante — dieselbe Auswahl wie beim Navigieren in
                // `reduce`. Neu gerechnet wird er (mit Diff) nur, wenn sich die
                // Kante selbst geändert hat oder `r` es verlangt.
                let inspector = match (inspector, chain.steps.get(cursor)) {
                    (Some(old), Some(WhyStep::Evidence { .. })) => {
                        let focused = links.get(edge).map(std::slice::from_ref).unwrap_or(&[]);
                        let same = |a: &[LinkEvidence], b: &[LinkEvidence]| {
                            a.len() == b.len()
                                && a.iter().zip(b).all(|(a, b)| {
                                    (a.commit, a.session, a.evidence)
                                        == (b.commit, b.session, b.evidence)
                                })
                        };
                        if !full && same(&old, focused) {
                            Some(old)
                        } else {
                            Some(self.inspection.explain_links(self.repo, focused))
                        }
                    }
                    _ => None,
                };
                Ok(View::Why {
                    origin,
                    chain,
                    cursor,
                    edge,
                    inspector,
                })
            }
        }
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
        self.push_why(
            WhyOrigin::Line {
                path: path.to_string(),
                line,
            },
            chain,
        );
        Ok(())
    }

    fn refilter(&mut self) {
        self.refilter_keeping(self.selected().map(|c| c.id));
    }

    /// Filtert neu und stellt den Cursor auf `keep`, wenn die Karte noch
    /// sichtbar ist — sonst an den Anfang.
    fn refilter_keeping(&mut self, keep: Option<SessionId>) {
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

    /// Die Zeilen des Graphen einer Session, wie `Enter` sie zeigen würde —
    /// aber ohne den Stapel zu berühren. Die Vorschau neben der Liste
    /// zeichnet daraus; sie darf keinen `Esc` kosten und folgt dem Zoom wie
    /// eine gelegte Ebene. `None`, wenn der Reader keinen Graphen hat.
    pub fn preview_graph(&self, id: SessionId) -> Option<Vec<Row>> {
        self.inspection
            .graph(id)
            .map(|graph| layout::rows(&graph, self.zoom))
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

    fn push_why(&mut self, origin: WhyOrigin, chain: WhyChain) {
        self.views.push(View::Why {
            origin,
            chain,
            cursor: 0,
            edge: 0,
            inspector: None,
        });
    }

    fn push_evidence(&mut self, id: SessionId) {
        let view = self.evidence_view(id, 0);
        self.views.push(view);
    }

    /// Die Evidence-Ebene einer Session, mit dem Cursor auf `cursor`
    /// (begrenzt auf die vorhandenen Sektionen).
    fn evidence_view(&self, id: SessionId, cursor: usize) -> View {
        self.evidence_view_with(id, cursor, None)
    }

    /// Wie [`evidence_view`](Self::evidence_view), mit schon gerechneten
    /// Abgleichen, wenn `artifacts` sie trägt — dann läuft kein `git`.
    fn evidence_view_with(
        &self,
        id: SessionId,
        cursor: usize,
        artifacts: Option<Vec<CommitArtifact>>,
    ) -> View {
        let report = self.inspection.evidence_report(id);
        let uninterpreted = self
            .inspection
            .card(id)
            .map(|card| card.uninterpreted_calls)
            .unwrap_or(0);
        // Strikt lesend: Der Reader liest nur Blobs der verknüpften Commits.
        let artifacts = artifacts.unwrap_or_else(|| self.inspection.artifacts(self.repo, id));
        // Legacy hat keine Sektionen — nur den einen ehrlichen Satz.
        let last = if report.is_some() {
            EVIDENCE_SECTIONS - 1
        } else {
            0
        };
        View::Evidence {
            id,
            report,
            uninterpreted,
            artifacts,
            cursor: cursor.min(last),
        }
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
        // Der Hinweis auf eine geschlossene Ebene gilt dem Moment des
        // Neuladens; mit der nächsten Taste ist er gelesen — mit jeder, auch
        // einer unbelegten, nur nicht mit `r`, das ihn gerade erst erzeugen
        // kann.
        if action != Action::Reload {
            self.closed = None;
        }
        if action == Action::Quit {
            self.quit = true;
            return;
        }
        if action == Action::Reload {
            // `reduce` bleibt ohne Quelle prüfbar: Es merkt sich nur den
            // Wunsch, `run` lädt. Auch über der Hilfe, die `r` ja nennt.
            self.reload_requested = true;
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
                            self.push_why(WhyOrigin::Session(id), chain);
                        }
                        return;
                    }
                    Action::Evidence => {
                        self.views.push(View::Graph {
                            id,
                            graph,
                            rows,
                            cursor,
                            timeline,
                        });
                        self.push_evidence(id);
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
                                    self.push_why(WhyOrigin::Session(id), chain);
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
                origin,
                chain,
                mut cursor,
                mut edge,
                mut inspector,
            }) => {
                let last = chain.steps.len().saturating_sub(1);
                // Wie viele Kanten das Glied unter `at` trägt — nur das
                // Evidence-Glied hat welche; überall sonst ist der
                // Sub-Cursor bedeutungslos (und bleibt 0).
                let links_of = |at: usize| match chain.steps.get(at) {
                    Some(WhyStep::Evidence { links }) => links.len(),
                    _ => 0,
                };
                let mut keep = true;
                // Der Inspector folgt dem Fokus: Bewegung schließt ihn und
                // öffnet ihn nur auf dem Evidence-Glied neu; Esc schließt
                // ihn, ohne dass er sofort wiederkommt.
                let mut follow = true;
                match action {
                    Action::Up => {
                        inspector = None;
                        // Erst über die Kanten des Evidence-Glieds, dann
                        // hinaus (#132) — von unten kommend landet der
                        // Fokus auf der letzten Kante.
                        if edge > 0 {
                            edge -= 1;
                        } else if cursor > 0 {
                            cursor -= 1;
                            edge = links_of(cursor).saturating_sub(1);
                        }
                    }
                    Action::Down => {
                        inspector = None;
                        if edge + 1 < links_of(cursor) {
                            edge += 1;
                        } else if cursor < last {
                            cursor += 1;
                            edge = 0;
                        }
                    }
                    Action::Home | Action::PageUp => {
                        inspector = None;
                        cursor = 0;
                        edge = 0;
                    }
                    Action::End | Action::PageDown => {
                        inspector = None;
                        cursor = last;
                        edge = 0;
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
                        // Enter auf einer Kante springt in die Why-Kette
                        // ihres Commits — dieselbe Bewegung wie auf dem
                        // COMMIT-Glied; Esc trägt über den View-Stack
                        // zurück (#132).
                        Some(WhyStep::Evidence { links }) if !links.is_empty() => {
                            let commit = links[edge.min(links.len() - 1)].commit;
                            self.views.push(View::Why {
                                origin,
                                chain,
                                cursor,
                                edge,
                                inspector,
                            });
                            let chain = self.inspection.why_commit(commit);
                            self.push_why(WhyOrigin::Commit(commit), chain);
                            return;
                        }
                        Some(WhyStep::Sessions { cards }) => {
                            if let Some(card) = cards.first() {
                                let id = card.id;
                                self.views.push(View::Why {
                                    origin,
                                    chain,
                                    cursor,
                                    edge,
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
                                origin,
                                chain,
                                cursor,
                                edge,
                                inspector,
                            });
                            let chain = self.inspection.why_commit(commit);
                            self.push_why(WhyOrigin::Commit(commit), chain);
                            return;
                        }
                        _ => {}
                    },
                    _ => {}
                }
                if keep {
                    // Die Erklärung gehört zum Fokus, nicht zum Enter: Steht
                    // der Cursor auf dem Evidence-Glied, wird die
                    // **fokussierte** Kante erklärt — bei vielen Commits
                    // bliebe das Panel sonst endlos (#132).
                    if follow
                        && inspector.is_none()
                        && let Some(WhyStep::Evidence { links }) = chain.steps.get(cursor)
                    {
                        let focused = links
                            .get(edge.min(links.len().saturating_sub(1)))
                            .map(std::slice::from_ref)
                            .unwrap_or(&[]);
                        inspector = Some(self.inspection.explain_links(self.repo, focused));
                    }
                    self.views.push(View::Why {
                        origin,
                        chain,
                        cursor,
                        edge,
                        inspector,
                    });
                }
            }
            Some(View::Evidence {
                id,
                report,
                uninterpreted,
                artifacts,
                mut cursor,
            }) => {
                // Legacy hat keine Sektionen — nur den einen ehrlichen Satz.
                let last = if report.is_some() {
                    EVIDENCE_SECTIONS - 1
                } else {
                    0
                };
                let mut keep = true;
                match action {
                    Action::Up => cursor = cursor.saturating_sub(1),
                    Action::Down => cursor = (cursor + 1).min(last),
                    Action::Home | Action::PageUp => cursor = 0,
                    Action::End | Action::PageDown => cursor = last,
                    Action::Back => keep = false,
                    _ => {}
                }
                if keep {
                    self.views.push(View::Evidence {
                        id,
                        report,
                        uninterpreted,
                        artifacts,
                        cursor,
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
                if let Some(id) = self.selected().filter(|c| !c.is_degraded()).map(|c| c.id)
                    && let Some(chain) = self.inspection.why_session(id)
                {
                    self.push_why(WhyOrigin::Session(id), chain);
                }
            }
            Action::Evidence => {
                if let Some(card) = self.selected().filter(|c| !c.is_degraded()) {
                    self.push_evidence(card.id);
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

    /// Fragt die Quelle nach ihrem Fingerabdruck und lädt neu, wenn er sich
    /// geändert hat — oder wenn `r` es verlangt hat.
    ///
    /// Von selbst wird nicht geladen, wenn der Abdruck unbestimmbar ist
    /// (`None`, die Fußzeile sagt dann „live off") oder wenn genau dieser
    /// Abdruck schon einmal scheiterte. Kehrt der Stand zum geladenen zurück,
    /// ist eine alte Fehlermeldung hinfällig — außer `r` scheiterte genau auf
    /// diesem Stand: Dann bleibt die Meldung bis zur nächsten Änderung, denn
    /// das Laden ist ja wirklich gescheitert.
    pub fn refresh(&mut self, source: &dyn Source) {
        let forced = std::mem::take(&mut self.reload_requested);
        if forced {
            self.full_next = true;
        }
        let stamp = source.stamp();
        self.live = stamp.is_some();
        if !forced {
            if stamp.is_none() || stamp == self.failed_stamp {
                return;
            }
            if stamp == self.stamp {
                self.reload_error = None;
                self.failed_stamp = None;
                return;
            }
        }
        self.reload(source.load(), stamp, SystemTime::now());
    }

    /// Die Schleife: zeichnen, Taste lesen, anwenden, bei Bedarf neu laden —
    /// bis `quit`.
    pub fn run(mut self, source: &dyn Source) -> std::io::Result<()> {
        let (_guard, mut terminal) = Guard::take()?;
        let mut checked = Instant::now();
        while !self.quit {
            terminal.draw(|frame| view::draw(frame, &mut self))?;
            if event::poll(TICK)?
                && let Event::Key(key) = event::read()?
                && key.kind != event::KeyEventKind::Release
            {
                let action = input::map(key, self.searching);
                self.reduce(action);
            }
            // `r` gehalten (Tastenwiederholung) lädt höchstens alle `REPEAT`
            // — jedes volle Laden kann Unterprozesse starten.
            let wanted = self.reload_requested && checked.elapsed() >= REPEAT;
            if !self.quit && (wanted || checked.elapsed() >= CHECK) {
                self.refresh(source);
                // Ab dem Ende gemessen: Ein langsames Laden lässt danach
                // immer eine volle Sekunde für Tasten.
                checked = Instant::now();
            }
        }
        Ok(())
    }
}

/// Der Grund, aus dem eine Ebene einer Session schließt: Es gibt sie im neuen
/// Stand nicht mehr (etwa nach `minds forget`).
fn gone(id: SessionId) -> String {
    format!("session {}… is gone", short(id))
}

/// Die Session gibt es noch, aber ohne Herkunftskette (degradiert).
fn no_chain(id: SessionId) -> String {
    format!("session {}… has no why chain", short(id))
}

fn short(id: SessionId) -> String {
    let id = id.to_string();
    id.get(..12).unwrap_or(&id).to_string()
}

/// Was die Abgleiche einer Session bestimmt, ohne I/O aus dem Index gelesen:
/// je beanspruchtem Commit die Sessions, die ihn tragen. Commits sind
/// unveränderlich — bleibt das gleich, bleibt der Abgleich gleich.
fn artifact_inputs(inspection: &Inspection, id: SessionId) -> Vec<(CommitId, Vec<SessionId>)> {
    let index = inspection.index();
    index
        .claimed_commits(id)
        .into_iter()
        .map(|commit| (commit, index.sessions_of(commit).to_vec()))
        .collect()
}

/// Kopf und Blame-Ergebnis einer Zeilen-Kette — `None`, wenn sie nicht die
/// Form hat, die `why_line` baut (dann wird voll neu gerechnet).
fn blamed(chain: &WhyChain) -> Option<(WhyStep, Option<CommitId>)> {
    match chain.steps.as_slice() {
        [head @ WhyStep::Line { .. }, WhyStep::Commit { id, .. }, ..] => Some((head.clone(), *id)),
        _ => None,
    }
}

/// `HH:MM:SSZ` — UTC, wie die übrigen Zeiten der Oberfläche; ohne
/// Datums-Crate keine Ortszeit.
pub(crate) fn clock(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        % 86_400;
    format!("{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}
