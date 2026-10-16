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
use minds_core::{ContentHash, SessionId};
use minds_git::{CommitId, Repo};
use minds_reader::Inspection;
use minds_reader::artifact::CommitArtifact;
use minds_reader::changes::ChangeSet;
use minds_reader::graph::{NodeKind, SessionGraph};
use minds_reader::model::{EvidenceReport, LinkEvidence, SessionCard, WhyChain, WhyStep};

use crate::changes::{ChangesState, Focus};
use crate::filter;
use crate::input::{self, Action};
use crate::intent::{IntentTab, bound_sessions, named_anchors};
use crate::layout::{self, Row, Zoom};
use crate::overview::{OverviewFocus, OverviewState, Selected};
use crate::term::Guard;
use crate::verify::VerifyState;
use crate::view;
use crate::{Background, CommitVerify, Source, Stamp};

/// Wie lange auf eine Taste gewartet wird, bevor neu gezeichnet wird
/// (Größenänderung des Terminals).
const TICK: Duration = Duration::from_millis(250);

/// Ab so vielen Zeilen in der Normalstufe startet ein Graph in der
/// Übersicht (C7).
pub const LARGE_GRAPH: usize = 40;

/// Wie oft die Quelle nach ihrem Fingerabdruck gefragt wird.
const CHECK: Duration = Duration::from_secs(1);

/// Wie oft der Verify-Tab nach einem Neuladen von selbst neu prüft.
pub(crate) const VERIFY_EVERY: Duration = Duration::from_secs(5);

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

/// Die Sektion ARTIFACT im Evidence-Report — Enter springt von dort in den
/// Changes-Tab.
pub const ARTIFACT_SECTION: usize = 2;

/// Die Tabs der Oberfläche, in Anzeige-Reihenfolge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Die Historie als Commit-Graph, an jedem Commit seine Sessions.
    Overview,
    /// Die Sessions: Liste, Graph, Why, Evidence.
    Sessions,
    /// Das Urteil über einen Commit auf einen Blick.
    Verify,
    /// Die Änderungen eines Commits als Diff.
    Changes,
    /// Die Intent-Anker und wer sich an sie gebunden nennt.
    Intent,
}

impl Tab {
    /// Alle Tabs, in Reihenfolge.
    pub const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Sessions,
        Tab::Verify,
        Tab::Changes,
        Tab::Intent,
    ];

    /// Das Kurzlabel im Kopf, wenn der Platz nicht reicht (A6).
    pub fn short(self) -> &'static str {
        match self {
            Tab::Overview => "Ovw",
            Tab::Sessions => "Ses",
            Tab::Verify => "Ver",
            Tab::Changes => "Chg",
            Tab::Intent => "Int",
        }
    }

    /// Der Name im Kopf.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Sessions => "Sessions",
            Tab::Verify => "Verify",
            Tab::Changes => "Changes",
            Tab::Intent => "Intent",
        }
    }
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
    /// Die erste sichtbare Zeile der Hilfe — sie scrollt (A6).
    pub help_scroll: u16,
    /// Ob die Schleife enden soll.
    pub quit: bool,
    /// Zeilen je Seite — setzt die Zeichenroutine.
    pub page: usize,
    /// Die Breite des Terminals beim letzten Zeichnen — ob Split passt.
    pub width: u16,
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
    /// Der sichtbare Tab.
    pub tab: Tab,
    /// Der Changes-Tab — erst beim ersten Öffnen geladen (er liest Blobs).
    pub changes: Option<ChangesState>,
    /// Der Verify-Tab — erst beim ersten Öffnen gebaut.
    pub verify: Option<VerifyState>,
    /// Der Intent-Tab — erst beim ersten Öffnen gebaut.
    pub intent: Option<IntentTab>,
    /// Der Overview-Tab — erst beim ersten Öffnen gebaut.
    pub overview: Option<OverviewState>,
    /// Ob Nerd-Font-Glyphen gezeichnet werden (runde Pillen, Icons).
    pub nerd: bool,
    /// `NO_COLOR` (C6): ohne fg/bg-Farben zeichnen, Modifier bleiben.
    pub no_color: bool,
    /// Der Tick des Spinners, solange eine Prüfung im Worker läuft (B6).
    pub spin: usize,
    /// Die Uhr für relative Zeiten in Listen (C4) — `run` stellt sie je
    /// Tick, Tests setzen sie fest.
    pub now: SystemTime,
    /// Der Navigationsverlauf (B2): je tabübergreifendem Sprung der
    /// Ursprung — Esc kehrt von der Ebene des Sprungs genau dorthin zurück.
    pub history: Vec<NavEntry>,
    /// Der aktive Commit (B3): die Auswahl der Übersicht, der Commit in
    /// Verify und in Changes — einer für alle drei; `[ ]` verschiebt ihn.
    pub active_commit: Option<CommitId>,
    /// Das Tamper-Banner: Sessions, die beim Start manipuliert waren oder
    /// durch ein Neuladen neu manipuliert sind. Esc schließt es, Enter
    /// öffnet die erste.
    pub tamper: Option<TamperAlert>,
}

/// Der Worker für Prüfungen (B6): ein Thread, der eine [`Background`]-
/// Prüfung besitzt; Aufträge und Ergebnisse laufen über `mpsc`-Kanäle. Der
/// Thread endet, sobald der Worker fallen gelassen wird.
pub struct Worker {
    requests: std::sync::mpsc::Sender<CommitId>,
    results: std::sync::mpsc::Receiver<(CommitId, Result<CommitVerify, String>)>,
    /// Der Commit, der gerade geprüft wird — eine Prüfung zur Zeit.
    pub busy: Option<CommitId>,
}

impl Worker {
    /// Startet den Thread.
    pub fn spawn(mut background: Box<dyn Background>) -> Self {
        let (requests, inbox) = std::sync::mpsc::channel::<CommitId>();
        let (outbox, results) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(commit) = inbox.recv() {
                let answer = background.verify(commit);
                if outbox.send((commit, answer)).is_err() {
                    break;
                }
            }
        });
        Self {
            requests,
            results,
            busy: None,
        }
    }
}

/// Ein Bild des ASCII-Spinners (B6), je Tick weiter.
pub fn spinner(tick: usize) -> char {
    ['|', '/', '-', '\\'][tick % 4]
}

/// Ein Eintrag des Navigationsverlaufs: wo ein tabübergreifender Sprung
/// begann (Tab, Ebenen, Cursor, Suche, der Zustand des Ursprungs-Tabs) und
/// wohin er führte.
#[derive(Debug, Clone)]
pub struct NavEntry {
    /// Der Ursprungs-Tab.
    pub tab: Tab,
    /// Die Ebenen des Sessions-Tabs beim Sprung.
    pub views: Vec<View>,
    /// Die Listenzeile beim Sprung.
    pub cursor: usize,
    /// Die Suche beim Sprung.
    pub query: String,
    /// Der Zustand des Ursprungs-Tabs (nur der eine).
    pub overview: Option<OverviewState>,
    /// …
    pub verify: Option<VerifyState>,
    /// …
    pub changes: Option<ChangesState>,
    /// …
    pub intent: Option<IntentTab>,
    /// Der aktive Commit beim Sprung.
    pub active_commit: Option<CommitId>,
    /// Wohin der Sprung führte.
    pub target: Tab,
    /// Wie viele Ebenen der Sprung im Ziel öffnete — Esc auf dieser Tiefe
    /// kehrt zurück, statt die Ebene nur zu schließen.
    pub base: usize,
}

impl NavEntry {
    /// Der Ort des Ursprungs für den Breadcrumb (siehe [`place`]).
    pub(crate) fn place(&self) -> Vec<String> {
        place(
            self.tab,
            &self.views,
            self.verify
                .as_ref()
                .and_then(|s| s.commit())
                .or(self.changes.as_ref().and_then(|s| s.commit()))
                .or(self.active_commit),
        )
    }
}

/// Was das Tamper-Banner meldet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TamperAlert {
    /// Die neu manipulierten Sessions, in Listenreihenfolge.
    pub sessions: Vec<SessionId>,
    /// Seit wann (`HH:MM:SSZ` des vorigen Ladens) — `None` beim Start.
    pub since: Option<String>,
}

/// Die Sessions, deren Seal-Material nicht mehr passt (TAMPERED), in
/// Listenreihenfolge.
fn tampered_of(cards: &[SessionCard]) -> Vec<SessionId> {
    cards
        .iter()
        .filter(|card| {
            matches!(
                &card.provenance,
                minds_reader::model::Provenance::Chained(state)
                    if state.verdict == minds_reader::model::EvidenceVerdict::Tampered
            )
        })
        .map(|card| card.id)
        .collect()
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
            help_scroll: 0,
            quit: false,
            page: 20,
            width: 0,
            stamp: None,
            reload_requested: false,
            full_next: false,
            loaded_at: clock(SystemTime::now()),
            reload_error: None,
            failed_stamp: None,
            live: true,
            closed: None,
            tab: Tab::Sessions,
            changes: None,
            verify: None,
            intent: None,
            overview: None,
            nerd: false,
            tamper: None,
            active_commit: None,
            history: Vec::new(),
            now: SystemTime::now(),
            spin: 0,
            no_color: false,
        };
        // Auch beim Start: Was schon manipuliert ist, meldet das Banner.
        let tampered = tampered_of(&app.cards);
        if !tampered.is_empty() {
            app.tamper = Some(TamperAlert {
                sessions: tampered,
                since: None,
            });
        }
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
        let was_tampered = tampered_of(&self.cards);
        self.cards = inspection.cards();
        // Neu manipuliert: das Banner, laut. Was schon gemeldet (und
        // vielleicht weggeklickt) war, kommt nicht wieder.
        let now_tampered = tampered_of(&self.cards);
        let fresh: Vec<SessionId> = now_tampered
            .iter()
            .copied()
            .filter(|id| !was_tampered.contains(id))
            .collect();
        if !fresh.is_empty() {
            self.tamper = Some(TamperAlert {
                sessions: fresh,
                since: Some(self.loaded_at.clone()),
            });
        } else if let Some(alert) = self.tamper.as_mut() {
            alert.sessions.retain(|id| now_tampered.contains(id));
            if alert.sessions.is_empty() {
                self.tamper = None;
            }
        }
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
        if self.verify.is_some() {
            self.refresh_verify(&before, full);
        }
        if self.changes.is_some() {
            self.refresh_changes(&before, full);
        }
        // Die Anker neu holen (Signatur, Beleg) — gedrosselt wie Verify,
        // außer nach `r`.
        if let Some(state) = self.intent.as_mut() {
            state.invalidate(full);
        }
        // Die Historie neu: ein begrenzter Walk in-process, kein `git` —
        // nur bei sichtbarem Tab, sonst beim nächsten Zeigen.
        if let Some(state) = self.overview.as_mut() {
            if self.tab == Tab::Overview {
                state.rebuild(&self.inspection, self.repo);
            } else {
                state.stale = true;
            }
        }
        self.stamp = stamp;
        self.loaded_at = clock(now);
        self.reload_error = None;
        self.failed_stamp = None;
    }

    /// Die Änderungen von `commit`, fertig für den Tab — ein Lesefehler wird
    /// zur Meldung (fail-soft).
    fn load_changes(&self, commit: CommitId) -> Result<ChangeSet, String> {
        self.inspection
            .changes(self.repo, commit)
            .map_err(|err| minds_reader::sanitize(&err.to_string()))
    }

    /// Öffnet den Changes-Tab — auf `commit`, sonst auf HEAD.
    pub fn open_changes(&mut self, commit: Option<CommitId>) {
        let mut commits = self.inspection.change_commits();
        let at = match commit {
            Some(commit) => commits
                .iter()
                .position(|c| *c == commit)
                .unwrap_or_else(|| {
                    commits.push(commit);
                    commits.len() - 1
                }),
            None => 0,
        };
        let set = match commits.get(at) {
            Some(commit) => self.load_changes(*commit),
            None => Err("no commit yet".into()),
        };
        self.changes = Some(ChangesState::new(commits, at, set));
        self.tab = Tab::Changes;
    }

    /// Öffnet den Verify-Tab — auf `commit`, sonst auf HEAD. Der Reader-Teil
    /// steht sofort; den signaturabhängigen holt `run` über die Quelle.
    pub fn open_verify(&mut self, commit: Option<CommitId>) {
        let mut commits = self.inspection.change_commits();
        let at = match commit {
            Some(commit) => commits
                .iter()
                .position(|c| *c == commit)
                .unwrap_or_else(|| {
                    commits.push(commit);
                    commits.len() - 1
                }),
            None => 0,
        };
        self.verify = Some(VerifyState::build(&self.inspection, self.repo, commits, at));
        self.tab = Tab::Verify;
    }

    /// Öffnet den Overview-Tab (beim ersten Mal gelesen).
    /// Der erklärte Bereich eines gelesenen Ankers — so, wie die Übersicht
    /// ihn nach `fill_overview` hält. `None`, solange er dort nicht gelesen
    /// ist oder er keinen Bereich erklärt: die Why-Box liest nichts nach.
    pub fn requirement_scope(&self, id: &minds_core::ContentHash) -> Option<&[String]> {
        let detail = self
            .overview
            .as_ref()?
            .requirements
            .get(id)?
            .as_ref()
            .ok()?;
        (!detail.scope.is_empty()).then_some(detail.scope.as_slice())
    }

    pub fn open_overview(&mut self) {
        match self.overview.as_mut() {
            None => self.overview = Some(OverviewState::build(&self.inspection, self.repo)),
            Some(state) if state.stale => state.rebuild(&self.inspection, self.repo),
            Some(_) => {}
        }
        self.tab = Tab::Overview;
    }

    /// Springt zum Graphen der Session `id` im Sessions-Tab. Eine laufende
    /// Suche wird geleert — sie filterte die Session sonst womöglich aus der
    /// Liste.
    fn goto_session(&mut self, id: SessionId) {
        if self.inspection.graph(id).is_some() {
            self.tab = Tab::Sessions;
            self.query.clear();
            self.refilter_keeping(Some(id));
            self.views.clear();
            self.push_graph(id);
        } else {
            self.closed = Some("that session is not readable".into());
        }
    }

    /// Die Tasten im Overview-Tab.
    fn reduce_overview(&mut self, action: Action) {
        let Some(state) = self.overview.as_mut() else {
            return;
        };
        let page = self.page.max(1);
        if state.focus == OverviewFocus::Sessions {
            let sessions = state.sessions();
            match action {
                Action::Up => state.session = state.session.saturating_sub(1),
                Action::Down => {
                    let shown = sessions.len().min(crate::overview::MAX_SESSION_LINES);
                    state.session = (state.session + 1).min(shown.saturating_sub(1));
                }
                Action::Back => {
                    state.focus = OverviewFocus::Graph;
                    state.detail_scroll = 0;
                }
                Action::Enter => match state.chosen_session() {
                    Some(id) => self.goto_session(id),
                    // Schmal ist das Detail eine Ebene — auch ohne Session:
                    // Enter zeigt dann, wie der Commit entstand (Diff).
                    None => {
                        if let Some(Selected::Commit(row)) = state.selected() {
                            let commit = row.id;
                            self.open_changes(Some(commit));
                        }
                    }
                },
                Action::Space => state.toggle_detail(),
                Action::PageUp => state.detail_scroll = state.detail_scroll.saturating_sub(10),
                Action::PageDown => state.detail_scroll = state.detail_scroll.saturating_add(10),
                _ => {}
            }
            return;
        }
        let at = state.cursor;
        match action {
            // Space klappt den Anforderungstext auf (C3.4); aufgeklappt
            // blättern PgUp/PgDn im Detail.
            Action::Space => state.toggle_detail(),
            Action::PageUp if state.requirement_open || state.prompt_open => {
                state.detail_scroll = state.detail_scroll.saturating_sub(10);
            }
            Action::PageDown if state.requirement_open || state.prompt_open => {
                state.detail_scroll = state.detail_scroll.saturating_add(10);
            }
            // Unter 100 Spalten ist das Detail eine eigene Ebene hinter
            // Enter (C3.4); Esc führt zurück zur Liste.
            Action::Enter if state.narrow && state.selected().is_some() => {
                state.focus = OverviewFocus::Sessions;
                state.session = 0;
                return;
            }
            // Über die gezeigten Zeilen — der Filter aus `/` blendet aus (B4).
            Action::Up
            | Action::Down
            | Action::PageUp
            | Action::PageDown
            | Action::Home
            | Action::End => {
                state.cursor = crate::filter::step(&state.shown(), state.cursor, &action, page);
            }
            Action::Enter => match state.selected() {
                Some(Selected::Wip(wip)) => {
                    let id = wip.id;
                    self.goto_session(id);
                    return;
                }
                Some(Selected::Commit(row)) => {
                    let commit = row.id;
                    match row.sessions.as_slice() {
                        [] => self.open_changes(Some(commit)),
                        [only] => {
                            let only = *only;
                            self.goto_session(only);
                        }
                        _ => {
                            state.focus = OverviewFocus::Sessions;
                            state.session = 0;
                        }
                    }
                    return;
                }
                None => {}
            },
            Action::Why => {
                if let Some(Selected::Commit(row)) = state.selected() {
                    let commit = row.id;
                    let chain = self.inspection.why_commit(commit);
                    self.tab = Tab::Sessions;
                    self.views.clear();
                    self.push_why(WhyOrigin::Commit(commit), chain);
                } else {
                    // Eine Session ohne Commit hat keine Kette (B4: sagen).
                    self.closed = Some("w: no commit here — Enter opens the session".into());
                }
                return;
            }
            _ => {}
        }
        if state.cursor != at {
            state.session = 0;
            state.requirement_open = false;
            state.prompt_open = false;
            state.prompt_clipped.set(false);
            state.detail_scroll = 0;
        }
    }

    /// Öffnet den Intent-Tab — auf dem Anker `want`, wenn es ihn gibt.
    pub fn open_intent(&mut self, want: Option<ContentHash>) {
        match self.intent.as_mut() {
            Some(state) => {
                if let Some(id) = &want {
                    state.select(id);
                }
                if state.pending {
                    state.urgent = true;
                }
            }
            None => self.intent = Some(IntentTab::new(want)),
        }
        self.tab = Tab::Intent;
    }

    /// Holt die Anker, wenn sie ausstehen — nur bei sichtbarem Tab (die
    /// Signaturprüfung startet ssh-keygen).
    pub fn fill_intents(&mut self, source: &dyn Source) {
        if self.tab != Tab::Intent {
            return;
        }
        if let Some(state) = self.intent.as_mut()
            && state.pending
        {
            let answer = source.intents(&named_anchors(&self.inspection));
            state.fill(answer);
        }
    }

    /// Die Tasten im Intent-Tab.
    fn reduce_intent(&mut self, action: Action) {
        let Some(state) = self.intent.as_mut() else {
            return;
        };
        let last = state.list().len().saturating_sub(1);
        let at = state.cursor;
        match action {
            Action::Up | Action::Down | Action::Home => {
                state.cursor = crate::filter::step(&state.shown(), state.cursor, &action, 1);
            }
            Action::End => state.cursor = last,
            // Bild hoch/runter blättert im Detail (Snapshot).
            Action::PageUp => {
                state.scroll = state.scroll.min(state.max_scroll.get()).saturating_sub(10);
            }
            Action::PageDown => {
                state.scroll = state.scroll.saturating_add(10).min(state.max_scroll.get());
            }
            // Esc (B1): keine Ebene zu schließen — der Rückweg zum Ursprung
            // kommt mit dem Navigationsverlauf (B2), sonst nichts.
            Action::Back => {}
            Action::Enter => {
                // Eine Session, die sich an den Anker gebunden nennt (die
                // kleinste Id): ihr Graph im Sessions-Tab.
                let first = state
                    .selected()
                    .and_then(|info| bound_sessions(&self.inspection, &info.id).first().copied());
                match first {
                    Some((id, _)) if self.inspection.graph(id).is_some() => {
                        self.tab = Tab::Sessions;
                        self.query.clear();
                        self.refilter_keeping(Some(id));
                        self.push_graph(id);
                    }
                    Some(_) => self.closed = Some("that session is not readable".into()),
                    None => self.closed = Some("no session names this anchor".into()),
                }
                return;
            }
            _ => {}
        }
        if state.cursor != at {
            state.scroll = 0;
        }
    }

    /// Rechnet die Conformance eines Commits der Übersicht, falls eine
    /// fehlt — im Leerlauf, eine je Aufruf. `true`, wenn gerechnet wurde.
    pub fn fill_conformance(&mut self, source: &dyn Source) -> bool {
        if self.tab != Tab::Overview {
            return false;
        }
        let Some(state) = self.overview.as_mut() else {
            return false;
        };
        let Some(commit) = state.conformance_due() else {
            return false;
        };
        let mark = source
            .conformance(commit)
            .map(|report| crate::overview::ConformanceMark::of(&report));
        state.conformance.insert(commit, mark);
        true
    }

    /// Liest im Leerlauf, was das Detail der Übersicht braucht (C3.4): die
    /// Anker des gewählten Commits (dieselbe Quelle und Prüfung wie der
    /// Intent-Tab), dann seine Zeilen (dieselbe Rechnung wie der
    /// Changes-Tab). Eins je Aufruf; `true`, wenn gelesen wurde.
    pub fn fill_overview(&mut self, source: &dyn Source) -> bool {
        if self.tab != Tab::Overview {
            return false;
        }
        let Some(state) = self.overview.as_mut() else {
            return false;
        };
        let Some(row) = state.agent_commit() else {
            return false;
        };
        let commit = row.id;
        let missing: Vec<ContentHash> = row
            .anchors
            .iter()
            .filter(|a| !state.requirements.contains_key(*a))
            .cloned()
            .collect();
        if !missing.is_empty() {
            match source.intents(&missing) {
                Ok(list) => {
                    for entry in list.entries {
                        state.requirements.insert(entry.id, entry.detail);
                    }
                    for id in missing {
                        state
                            .requirements
                            .entry(id)
                            .or_insert_with(|| Err("anchor not read".into()));
                    }
                }
                Err(why) => {
                    for id in missing {
                        state.requirements.insert(id, Err(why.clone()));
                    }
                }
            }
            return true;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = state.lines.entry(commit) {
            let count = match self.inspection.changes(self.repo, commit) {
                Ok(set) => match &set.unassessed {
                    Some(why) => Err(why.clone()),
                    None => Ok(set.line_classes()),
                },
                Err(err) => Err(minds_reader::sanitize(&err.to_string())),
            };
            slot.insert(count);
            return true;
        }
        false
    }

    /// Prüft den gewählten Commit der Übersicht synchron (ohne Worker) —
    /// `true`, wenn geprüft wurde.
    pub fn fill_overview_verify(&mut self, source: &dyn Source) -> bool {
        if self.tab != Tab::Overview {
            return false;
        }
        let now = self.now_secs();
        let Some(state) = self.overview.as_mut() else {
            return false;
        };
        let Some(commit) = state.check_due() else {
            return false;
        };
        state.checks.insert(commit, source.verify(commit));
        state.checked_at.insert(commit, now);
        true
    }

    /// Gibt die ausstehende Prüfung des gewählten Commits der Übersicht an
    /// den Worker (B6) — `true`, wenn sie unterwegs ist.
    pub fn send_overview_check(&mut self, worker: &mut Worker) -> bool {
        if worker.busy.is_some() || self.tab != Tab::Overview {
            return false;
        }
        let Some(state) = self.overview.as_mut() else {
            return false;
        };
        let Some(commit) = state.check_due() else {
            return false;
        };
        if worker.requests.send(commit).is_err() {
            return false;
        }
        state.asked = Some(commit);
        worker.busy = Some(commit);
        true
    }

    /// Holt den signaturabhängigen Teil, wenn er aussteht — nur hier, mit
    /// der Quelle; `reduce` merkt sich nur den Bedarf.
    /// Nur bei sichtbarem Tab: Ein verdeckter Tab startet kein ssh-keygen.
    /// Gibt die ausstehende Prüfung des gezeigten Commits an den Worker —
    /// `true`, wenn sie unterwegs ist. Die Prüfung bleibt „pending": Der Tab
    /// zeigt „checking", bis das Ergebnis da ist (B6).
    pub fn send_check(&mut self, worker: &mut Worker) -> bool {
        if worker.busy.is_some() {
            return false;
        }
        let Some(state) = self.verify.as_mut().filter(|s| s.pending) else {
            return false;
        };
        let Some(commit) = state.commit() else {
            return false;
        };
        state.urgent = false;
        if worker.requests.send(commit).is_err() {
            return false;
        }
        worker.busy = Some(commit);
        true
    }

    /// Übernimmt fertige Prüfungen des Workers — `true`, wenn eine kam. Ein
    /// Ergebnis für einen inzwischen anderen Commit wird verworfen; dessen
    /// Prüfung steht dann weiter aus.
    pub fn take_checks(&mut self, worker: &mut Worker) -> bool {
        let Ok((commit, answer)) = worker.results.try_recv() else {
            return false;
        };
        worker.busy = None;
        // Die Übersicht nimmt nur die Antwort, nach der dieser Stand gefragt
        // hat — ein Neuladen baut sie ohne Frage neu (C3.4, B6).
        let now = self.now_secs();
        if let Some(state) = self.overview.as_mut()
            && state.asked == Some(commit)
        {
            state.asked = None;
            state.checks.insert(commit, answer.clone());
            state.checked_at.insert(commit, now);
        }
        if let Some(state) = self.verify.as_mut()
            && state.commit() == Some(commit)
        {
            state.pending = false;
            state.cli = Some(answer);
        }
        true
    }

    pub fn fill_verify(&mut self, source: &dyn Source) {
        if self.tab != Tab::Verify {
            return;
        }
        if let Some(state) = self.verify.as_mut()
            && state.pending
        {
            state.pending = false;
            state.cli = state.commit().map(|commit| source.verify(commit));
        }
    }

    /// Der Verify-Tab nach dem Neuladen: derselbe Commit, dieselbe Zeile.
    /// Der signaturabhängige Teil wird **immer** neu geprüft: Er hängt an
    /// Material, das die Session-Ids nicht abbilden (eine ausgetauschte
    /// `seal.sig`, neue Seals, der Intent-Anker) — ein übernommenes Urteil
    /// könnte VERIFIED zeigen, wo `verify` TAMPERED sagt. Geprüft wird erst,
    /// wenn der Tab sichtbar ist.
    fn refresh_verify(&mut self, before: &Inspection, full: bool) {
        let Some(state) = self.verify.take() else {
            return;
        };
        let mut commits = self.inspection.change_commits();
        let kept = state.commit().map(|commit| {
            commits
                .iter()
                .position(|c| *c == commit)
                .unwrap_or_else(|| {
                    commits.push(commit);
                    commits.len() - 1
                })
        });
        // Der Abgleich (git diff-tree) nur neu, wenn sich Commit oder
        // Claimants geändert haben oder das Neuladen voll ist — wie im
        // Changes-Tab: Wer nur Refs schreibt, startet kein `git`.
        let reuse = match state.commit() {
            Some(commit)
                if !full
                    && commit_inputs(before, commit) == commit_inputs(&self.inspection, commit) =>
            {
                Some(state.artifact.clone())
            }
            _ => None,
        };
        let mut next = VerifyState::build_with(
            &self.inspection,
            self.repo,
            commits,
            kept.unwrap_or(0),
            reuse,
        );
        // Von selbst gedrosselt (`VERIFY_EVERY`) — außer `r` oder eine noch
        // ausstehende Prüfung, die der Nutzer verlangt hatte.
        next.urgent = full || (state.pending && state.urgent);
        next.cursor = state.cursor.min(next.rows().saturating_sub(1));
        self.verify = Some(next);
    }

    /// Die Tasten im Verify-Tab.
    fn reduce_verify(&mut self, action: Action) {
        let Some(state) = self.verify.as_mut() else {
            return;
        };
        let page = self.page.max(1);
        let last = state.rows().saturating_sub(1);
        match action {
            Action::Up => state.cursor = state.cursor.saturating_sub(1),
            Action::Down => state.cursor = (state.cursor + 1).min(last),
            Action::Home => state.cursor = 0,
            Action::End => state.cursor = last,
            // Bild hoch/runter blättern seitenweise, wie überall (B4).
            Action::PageUp => state.cursor = state.cursor.saturating_sub(page),
            Action::PageDown => state.cursor = (state.cursor + page).min(last),
            Action::Bracket(forward) => {
                let at = if forward {
                    (state.at + 1).min(state.commits.len().saturating_sub(1))
                } else {
                    state.at.saturating_sub(1)
                };
                if at != state.at {
                    let commits = state.commits.clone();
                    self.verify =
                        Some(VerifyState::build(&self.inspection, self.repo, commits, at));
                }
            }
            // Esc (B1): keine Ebene zu schließen — sonst nichts.
            Action::Back => {}
            Action::Enter => {
                let cursor = state.cursor;
                let commit = state.commit();
                if cursor < state.sessions.len() {
                    // Eine Session: ihr Graph im Sessions-Tab.
                    let id = state.sessions[cursor].id;
                    if self.inspection.graph(id).is_some() {
                        self.tab = Tab::Sessions;
                        self.query.clear();
                        self.refilter_keeping(Some(id));
                        self.push_graph(id);
                    } else {
                        // Nicht still in eine leere Liste springen.
                        self.closed = Some("that session is not readable".into());
                    }
                } else if cursor == state.artifact_row() {
                    self.open_changes(commit);
                } else {
                    // Scope: die erste Datei außerhalb des Bereichs im Diff.
                    // Der rohe Pfad (nur zum Wählen) — der entschärfte träfe
                    // eine Datei mit Steuerzeichen im Namen nicht.
                    let paths: Vec<String> = state
                        .checked()
                        .map(|c| c.out_of_scope_paths.clone())
                        .unwrap_or_default();
                    self.open_changes(commit);
                    // Bevorzugt eine Datei, die im Commit liegt — die Liste
                    // nennt auch nur Geschriebenes (und wieder Gelöschtes).
                    if let Some(changes) = self.changes.as_mut()
                        && !paths.iter().any(|path| changes.select_path(path))
                    {
                        changes.file = 0;
                    }
                }
            }
            _ => {}
        }
    }

    /// Zeigt `tab`; Changes und Verify laden beim ersten Mal.
    fn show_tab(&mut self, tab: Tab) {
        // Der aktive Commit gilt in jedem Tab, der einen Commit zeigt (B3).
        let active = self.active_commit;
        let differs = |shown: Option<CommitId>| active.is_some() && shown != active;
        if tab == Tab::Changes
            && (self.changes.is_none() || differs(self.changes.as_ref().and_then(|s| s.commit())))
        {
            let keep = self.changes.as_ref().map(|s| (s.split, s.why));
            self.open_changes(active);
            if let (Some((split, why)), Some(state)) = (keep, self.changes.as_mut()) {
                state.split = split;
                state.why = why;
            }
        } else if tab == Tab::Verify
            && (self.verify.is_none() || differs(self.verify.as_ref().and_then(|s| s.commit())))
        {
            self.open_verify(active);
        } else if tab == Tab::Intent {
            self.open_intent(None);
        } else if tab == Tab::Overview {
            self.open_overview();
            if let (Some(commit), Some(state)) = (active, self.overview.as_mut()) {
                state.select_commit(commit);
            }
        } else {
            // Zurück im Verify-Tab: Eine ausstehende Prüfung ist jetzt vom
            // Nutzer verlangt, nicht gedrosselt.
            if tab == Tab::Verify
                && let Some(state) = self.verify.as_mut()
                && state.pending
            {
                state.urgent = true;
            }
            self.tab = tab;
        }
    }

    /// Der Changes-Tab nach dem Neuladen: derselbe Commit, dieselbe Datei,
    /// dieselbe Stelle, wenn es sie noch gibt. Neu gelesen wird der Commit
    /// nur, wenn sich seine Eingaben geändert haben (Sessions, Trailer) oder
    /// das Neuladen voll ist — sonst bestimmte, wer Refs schreibt, wie oft
    /// Blobs gelesen werden. Der Review-Stand wird immer aufgefrischt.
    fn refresh_changes(&mut self, before: &Inspection, full: bool) {
        let Some(state) = self.changes.take() else {
            return;
        };
        let mut commits = self.inspection.change_commits();
        // Ein Commit außerhalb der HEAD-Historie (über ARTIFACT geöffnet,
        // etwa ein durch Amend ersetzter) bleibt, wie `open_changes` ihn
        // angehängt hat — das Neuladen springt nicht still auf HEAD.
        let kept = state.commit().map(|commit| {
            commits
                .iter()
                .position(|c| *c == commit)
                .unwrap_or_else(|| {
                    commits.push(commit);
                    commits.len() - 1
                })
        });
        let at = kept.unwrap_or(0);
        let commit = commits.get(at).copied();
        // Ein früherer Lesefehler wird erneut versucht, nicht weitergereicht.
        let unchanged = kept.is_some()
            && !full
            && state.set.is_ok()
            && commit
                .is_some_and(|c| commit_inputs(before, c) == commit_inputs(&self.inspection, c));
        // Was nach dem Bau noch gebraucht wird — dann darf `set` umziehen,
        // statt bis zu 200 000 Zeilen zu kopieren.
        let path = state.current().map(|f| f.identity.clone());
        let (split, why, focus, row) = (state.split, state.why, state.focus, state.row);
        let set = match commit {
            Some(c) if unchanged => state.set.map(|mut set| {
                set.review = self.inspection.review_state_of_commit(c);
                set
            }),
            Some(c) => self.load_changes(c),
            None => Err("no commit yet".into()),
        };
        let mut next = ChangesState::new(commits, at, set);
        next.split = split;
        next.why = why;
        // Stelle und Fokus nur, wenn es die Datei noch gibt — sonst gehörte
        // die alte Zeile zu einer anderen Datei.
        if kept.is_some()
            && let Some(path) = path
            && next.select_path(&path)
        {
            next.focus = focus;
            next.row = row;
            next.move_row(0);
        }
        self.changes = Some(next);
    }

    /// Ein Commit weiter (`forward`: älter) oder zurück.
    fn step_commit(&mut self, forward: bool) {
        let Some(state) = &self.changes else {
            return;
        };
        let at = if forward {
            (state.at + 1).min(state.commits.len().saturating_sub(1))
        } else {
            state.at.saturating_sub(1)
        };
        if at == state.at {
            return;
        }
        let commit = state.commits[at];
        let set = self.load_changes(commit);
        let (split, why) = (state.split, state.why);
        let mut next = ChangesState::new(state.commits.clone(), at, set);
        next.split = split;
        next.why = why;
        self.changes = Some(next);
    }

    /// Die Tasten im Changes-Tab.
    fn reduce_changes(&mut self, action: Action) {
        let page = self.page.max(1) as isize;
        let split_shown = self.width >= crate::view::changes::SPLIT_MIN;
        // Unter 120 Spalten ist „Warum diese Zeile?" eine Ebene (C9.2).
        let why_as_layer = self.width < crate::view::changes::WHY_LAYER_MAX;
        let Some(state) = self.changes.as_mut() else {
            return;
        };
        match (state.focus, action) {
            (_, Action::Split) => state.split = !state.split,
            (_, Action::Context) => state.why = !state.why,
            (_, Action::Unexplained(forward)) => {
                if state.jump_unexplained(forward) {
                    state.focus = Focus::Diff;
                }
            }
            (Focus::Files, Action::Up) => state.move_file(false),
            (Focus::Files, Action::Down) => state.move_file(true),
            (Focus::Files, Action::Home | Action::PageUp) => {
                state.file = 0;
                state.row = 0;
            }
            (Focus::Files, Action::End | Action::PageDown) => {
                state.file = state.files().len().saturating_sub(1);
                state.row = 0;
            }
            (Focus::Files, Action::Enter) => {
                if let Some(file) = state.current().filter(|f| !f.rows.is_empty()) {
                    // Gleich auf die erste Änderung, nicht auf den Kopf.
                    state.row = file
                        .rows
                        .iter()
                        .position(|r| r.kind != minds_git::DiffKind::Hunk)
                        .unwrap_or(0);
                    state.focus = Focus::Diff;
                }
            }
            (Focus::Files, Action::Bracket(forward)) => self.step_commit(forward),
            // Esc (B1): die Dateiliste ist die oberste Ebene — sonst nichts.
            (Focus::Files, Action::Back) => {}
            // Im gezeigten Split paarweise, wie gezeichnet.
            (Focus::Diff, Action::Up) if state.split && split_shown => state.move_pair(-1),
            (Focus::Diff, Action::Down) if state.split && split_shown => state.move_pair(1),
            (Focus::Diff, Action::Up) => state.move_row(-1),
            (Focus::Diff, Action::Down) => state.move_row(1),
            (Focus::Diff, Action::PageUp) if state.split && split_shown => state.move_pair(-page),
            (Focus::Diff, Action::PageDown) if state.split && split_shown => state.move_pair(page),
            (Focus::Diff, Action::PageUp) => state.move_row(-page),
            (Focus::Diff, Action::PageDown) => state.move_row(page),
            (Focus::Diff, Action::Home) => state.row_to(false),
            (Focus::Diff, Action::End) => state.row_to(true),
            (Focus::Diff, Action::Bracket(forward)) => state.jump_hunk(forward),
            // Esc (B1): erst die Ebene zu, dann in die Dateiliste.
            (Focus::Diff, Action::Back) if state.why_layer => state.why_layer = false,
            (Focus::Diff, Action::Back) => state.focus = Focus::Files,
            (Focus::Diff, Action::Enter) if why_as_layer => state.why_layer = !state.why_layer,
            (Focus::Diff, Action::Enter) => self.goto_tool_call(),
            _ => {}
        }
    }

    /// Enter auf einer Codezeile (C9.3): der Graph der Session, die sie
    /// schrieb, mit dem Cursor auf genau diesem Tool-Aufruf — der Rückweg
    /// führt per Esc über den Verlauf (Paket 2). Die Zuordnung Zeile →
    /// (Session, Turn, Aufruf) ist [`minds_reader::reconcile::LineSource`];
    /// ohne sie sagt die Fußzeile es (B4).
    fn goto_tool_call(&mut self) {
        let source = self.changes.as_ref().and_then(|state| {
            state
                .current()
                .and_then(|f| f.rows.get(state.row))
                .and_then(|r| r.source)
        });
        let Some(source) = source else {
            self.closed = Some(
                "Enter: no tool call is recorded for this line — w opens the why chain".into(),
            );
            return;
        };
        let Some(graph) = self.inspection.graph(source.session) else {
            self.closed = Some("that session is not readable".into());
            return;
        };
        // Der Knoten des Aufrufs: der `call`-te Tool-Knoten unter dem
        // `turn`-ten Zug — so baut `SessionGraph::build` den Graphen.
        let turn = graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Turn(_)))
            .nth(source.turn)
            .map(|n| n.id);
        let node = turn.and_then(|turn| {
            graph
                .nodes
                .iter()
                .filter(|n| n.parent == Some(turn) && matches!(n.kind, NodeKind::Tool(_)))
                .nth(source.call)
                .map(|n| n.id)
        });
        self.goto_session(source.session);
        if let (Some(node), Some(View::Graph { rows, cursor, .. })) = (node, self.views.last_mut())
            && let Some(at) = rows
                .iter()
                .position(|r| r.node <= node && node < r.node + r.count)
        {
            *cursor = at;
        }
    }

    /// `w` auf einer Diff-Zeile: die Herkunftskette dieser Zeile — im
    /// Sessions-Tab, `Esc` führt zurück in die Liste. Auf HEAD die Kette der
    /// Zeile (Blame), auf älteren Commits die des Commits.
    fn why_of_row(&mut self) {
        // Nie still nichts (B4): Fehlt Commit oder Zeile, sagt die Fußzeile es.
        let Some(commit) = self.changes.as_ref().and_then(|s| s.commit()) else {
            self.closed = Some("w: no commit here to explain".into());
            return;
        };
        let target = self.changes.as_ref().and_then(|state| {
            state
                .current()
                .and_then(|f| f.rows.get(state.row).map(|r| (f.identity.clone(), r.new)))
        });
        let Some((path, line)) = target else {
            self.closed = Some("w: no line here — open a file's diff first".into());
            return;
        };
        match line {
            Some(line) if self.inspection.head() == Some(commit) => {
                match self.inspection.why_line(self.repo, &path, line) {
                    Ok(chain) => self.push_why(WhyOrigin::Line { path, line }, chain),
                    Err(err) => {
                        self.closed =
                            Some(minds_reader::sanitize(&format!("why {path}:{line}: {err}")));
                        return;
                    }
                }
            }
            _ => {
                let chain = self.inspection.why_commit(commit);
                self.push_why(WhyOrigin::Commit(commit), chain);
            }
        }
        self.tab = Tab::Sessions;
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
        // Ein großer Graph startet in der Übersicht (C7) — die Fußzeile
        // sagt, dass mehr da ist.
        if self.zoom == Zoom::Normal && layout::rows(&graph, Zoom::Normal).len() > LARGE_GRAPH {
            self.zoom = Zoom::Summary;
            self.relayout();
        }
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
                // Der Cursor folgt dem Knoten, nicht dem Index (B5).
                let node = rows.get(*cursor).map(|r| r.node);
                *rows = if *timeline {
                    layout::timeline(graph, zoom)
                } else {
                    layout::rows(graph, zoom)
                };
                *cursor = node
                    .and_then(|n| layout::locate(graph, zoom, rows, n))
                    .unwrap_or_else(|| (*cursor).min(rows.len().saturating_sub(1)));
            }
        }
    }

    /// Wendet eine Aktion an — und übernimmt danach den Commit des
    /// sichtbaren Tabs als aktiven (B3).
    pub fn reduce(&mut self, action: Action) {
        // Auch vorher: Die Startauswahl (HEAD) ist schon aktiv, bevor die
        // erste Taste fällt.
        self.sync_active();
        // Esc, Schritt 4 (B1/B2): auf der Ebene eines Sprungs zurück zum
        // Ursprung — mit Ebenen und Cursor.
        if action == Action::Back && self.at_jump_base() {
            self.go_back();
        } else {
            // Nur Enter und die Objekt-Verben springen über Tab-Grenzen; nur
            // dann lohnt der Schnappschuss.
            let origin = matches!(
                action,
                Action::Enter
                    | Action::Why
                    | Action::Evidence
                    | Action::Verify
                    | Action::Diff
                    | Action::Intent
            )
            .then(|| self.snapshot());
            let tab_key = matches!(action, Action::TabTo(_) | Action::CycleTab(_));
            self.reduce_action(action);
            if let Some(mut entry) = origin
                && entry.tab != self.tab
                && !self.quit
            {
                entry.target = self.tab;
                entry.base = if self.tab == Tab::Sessions {
                    self.views.len()
                } else {
                    0
                };
                self.remember(entry);
            }
            // Ein Tab-Wechsel per Taste (F1–F5, 1–5, Tab) auf einen Tab, der
            // im Verlauf als Ursprung liegt, beendet dessen Sprung: der Nutzer
            // ist zurück am Ursprung, alles ab diesem Eintrag fällt weg.
            if tab_key && let Some(at) = self.history.iter().position(|e| e.tab == self.tab) {
                self.history.truncate(at);
            }
        }
        self.sync_active();
    }

    /// Die Uhr in Sekunden seit Epoch (C4).
    pub fn now_secs(&self) -> i64 {
        self.now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Übernimmt den Commit des sichtbaren Tabs als aktiven (B3).
    fn sync_active(&mut self) {
        let shown = match self.tab {
            Tab::Overview => match self.overview.as_ref().and_then(|s| s.selected()) {
                Some(crate::overview::Selected::Commit(row)) => Some(row.id),
                _ => None,
            },
            Tab::Verify => self.verify.as_ref().and_then(|s| s.commit()),
            Tab::Changes => self.changes.as_ref().and_then(|s| s.commit()),
            Tab::Sessions | Tab::Intent => None,
        };
        if shown.is_some() {
            self.active_commit = shown;
        }
    }

    /// Der Gegenstand unter dem Fokus — Session und/oder Commit (B4).
    fn focus_object(&self) -> (Option<SessionId>, Option<CommitId>) {
        let commit_of = |id: SessionId| {
            self.inspection
                .card(id)
                .and_then(|card| card.commits.first().copied())
        };
        match self.tab {
            Tab::Sessions => {
                let session = match self.top() {
                    Some(View::Graph { id, .. }) | Some(View::Evidence { id, .. }) => Some(*id),
                    Some(View::Why { origin, .. }) => match origin {
                        WhyOrigin::Session(id) => Some(*id),
                        _ => None,
                    },
                    None => self.selected().filter(|c| !c.is_degraded()).map(|c| c.id),
                };
                let commit = match self.top() {
                    Some(View::Why {
                        origin: WhyOrigin::Commit(commit),
                        ..
                    }) => Some(*commit),
                    _ => session.and_then(commit_of),
                };
                (session, commit)
            }
            Tab::Overview => match self.overview.as_ref().and_then(|s| s.selected()) {
                Some(Selected::Commit(row)) => (
                    self.overview
                        .as_ref()
                        .and_then(|s| s.chosen_session())
                        .or(row.sessions.first().copied()),
                    Some(row.id),
                ),
                Some(Selected::Wip(wip)) => (Some(wip.id), None),
                None => (None, None),
            },
            Tab::Verify => {
                let state = self.verify.as_ref();
                let session = state
                    .and_then(|s| s.sessions.get(s.cursor).map(|row| row.id))
                    .or_else(|| state.and_then(|s| s.sessions.first().map(|row| row.id)));
                (session, state.and_then(|s| s.commit()))
            }
            Tab::Changes => {
                let commit = self.changes.as_ref().and_then(|s| s.commit());
                let session =
                    commit.and_then(|c| self.inspection.index().sessions_of(c).first().copied());
                (session, commit)
            }
            Tab::Intent => {
                let session = self.intent.as_ref().and_then(|state| {
                    state.selected().and_then(|anchor| {
                        crate::intent::bound_sessions(&self.inspection, &anchor.id)
                            .first()
                            .map(|(id, _)| *id)
                    })
                });
                (session, session.and_then(commit_of))
            }
        }
    }

    /// Ein Objekt-Verb auf dem Gegenstand unter dem Fokus — oder ein kurzer
    /// Hinweis, warum es hier nicht passt (B4: nie still nichts).
    fn verb(&mut self, action: Action) {
        let (session, commit) = self.focus_object();
        match action {
            // Im Verify-Tab selbst: dieselbe Prüfung jetzt noch einmal (C5).
            Action::Verify if self.tab == Tab::Verify && self.verify.is_some() => {
                if let Some(state) = self.verify.as_mut() {
                    state.pending = true;
                    state.urgent = true;
                }
            }
            Action::Verify => match commit {
                Some(commit) => self.open_verify(Some(commit)),
                None => self.closed = Some("v: no commit here to verify".into()),
            },
            Action::Diff => match commit {
                Some(commit) => {
                    let from_overview = self.tab == Tab::Overview;
                    self.open_changes(Some(commit));
                    // Aus der Übersicht (`→ d, then n`, C9.1): auf die erste
                    // Zeile im offenen Zustand, nicht auf die erste Codezeile.
                    if from_overview && let Some(state) = self.changes.as_mut() {
                        state.land_on_open();
                    }
                }
                None => self.closed = Some("d: no commit here to diff".into()),
            },
            Action::Intent => {
                let anchor = session.and_then(|id| {
                    crate::intent::named_anchors(&self.inspection)
                        .into_iter()
                        .find(|a| {
                            crate::intent::bound_sessions(&self.inspection, a)
                                .iter()
                                .any(|(s, _)| *s == id)
                        })
                });
                match anchor {
                    Some(anchor) => self.open_intent(Some(anchor)),
                    None => {
                        self.closed = Some("i: no intent anchor is bound to this session".into())
                    }
                }
            }
            Action::Why if self.tab == Tab::Changes => self.why_of_row(),
            Action::Why => match (commit, session) {
                (Some(commit), _) => {
                    let chain = self.inspection.why_commit(commit);
                    self.tab = Tab::Sessions;
                    self.views.clear();
                    self.push_why(WhyOrigin::Commit(commit), chain);
                }
                (None, Some(id)) => match self.inspection.why_session(id) {
                    Some(chain) => {
                        self.tab = Tab::Sessions;
                        self.views.clear();
                        self.push_why(WhyOrigin::Session(id), chain);
                    }
                    None => self.closed = Some("w: this session is not readable".into()),
                },
                (None, None) => self.closed = Some("w: nothing here to explain".into()),
            },
            Action::Evidence => match session {
                Some(id) => {
                    self.tab = Tab::Sessions;
                    self.query.clear();
                    self.refilter_keeping(Some(id));
                    self.views.clear();
                    self.push_evidence(id);
                }
                None => self.closed = Some("e: no session here".into()),
            },
            Action::Context => {
                self.closed = Some("c: the why column belongs to the Changes tab".into())
            }
            _ => {}
        }
    }

    /// Der Filter aus `/` des sichtbaren Tabs (B4); Sessions nutzen `query`.
    pub fn tab_filter(&self) -> Option<&str> {
        match self.tab {
            Tab::Overview => self.overview.as_ref().map(|s| s.filter.as_str()),
            Tab::Changes => self.changes.as_ref().map(|s| s.filter.as_str()),
            Tab::Intent => self.intent.as_ref().map(|s| s.filter.as_str()),
            Tab::Sessions | Tab::Verify => None,
        }
    }

    fn tab_filter_mut(&mut self) -> Option<&mut String> {
        match self.tab {
            Tab::Overview => self.overview.as_mut().map(|s| &mut s.filter),
            Tab::Changes => self.changes.as_mut().map(|s| &mut s.filter),
            Tab::Intent => self.intent.as_mut().map(|s| &mut s.filter),
            Tab::Sessions | Tab::Verify => None,
        }
    }

    /// Der Cursor des Tabs auf einen Treffer des Filters.
    fn refit_tab(&mut self) {
        match self.tab {
            Tab::Overview => {
                if let Some(state) = self.overview.as_mut() {
                    let cursor = crate::filter::refit(&state.shown(), state.cursor);
                    if cursor != state.cursor {
                        state.cursor = cursor;
                        state.session = 0;
                    }
                }
            }
            Tab::Changes => {
                if let Some(state) = self.changes.as_mut() {
                    let file = crate::filter::refit(&state.shown(), state.file);
                    if file != state.file {
                        state.file = file;
                        state.row = 0;
                    }
                }
            }
            Tab::Intent => {
                if let Some(state) = self.intent.as_mut() {
                    state.cursor = crate::filter::refit(&state.shown(), state.cursor);
                }
            }
            Tab::Sessions | Tab::Verify => {}
        }
    }

    /// ↑↓ beim Tippen eines Filters: über die Treffer.
    fn step_tab(&mut self, action: &Action) {
        match self.tab {
            Tab::Overview => {
                if let Some(state) = self.overview.as_mut() {
                    state.cursor = crate::filter::step(&state.shown(), state.cursor, action, 1);
                }
            }
            Tab::Changes => {
                if let Some(state) = self.changes.as_mut() {
                    state.move_file(*action == Action::Down);
                }
            }
            Tab::Intent => {
                if let Some(state) = self.intent.as_mut() {
                    state.cursor = crate::filter::step(&state.shown(), state.cursor, action, 1);
                }
            }
            Tab::Sessions | Tab::Verify => {}
        }
    }

    /// Höchstens so viele Ursprünge merkt sich der Verlauf; die ältesten
    /// fallen weg.
    pub(crate) const HISTORY_CAP: usize = 8;

    /// Legt einen Ursprung ab — außer er gleicht dem letzten Eintrag (Tab und
    /// Ort): dann ist der Sprung derselbe, der Breadcrumb zeigt den Ort nicht
    /// zweimal. Über [`Self::HISTORY_CAP`] hinaus fällt der älteste Eintrag.
    pub(crate) fn remember(&mut self, entry: NavEntry) {
        if self
            .history
            .last()
            .is_some_and(|last| last.tab == entry.tab && last.place() == entry.place())
        {
            return;
        }
        self.history.push(entry);
        if self.history.len() > Self::HISTORY_CAP {
            let excess = self.history.len() - Self::HISTORY_CAP;
            self.history.drain(..excess);
        }
    }

    /// Der Ursprung, wie er gerade ist.
    pub(crate) fn snapshot(&self) -> NavEntry {
        NavEntry {
            tab: self.tab,
            views: self.views.clone(),
            cursor: self.cursor,
            query: self.query.clone(),
            overview: (self.tab == Tab::Overview)
                .then(|| self.overview.clone())
                .flatten(),
            verify: (self.tab == Tab::Verify)
                .then(|| self.verify.clone())
                .flatten(),
            changes: (self.tab == Tab::Changes)
                .then(|| self.changes.clone())
                .flatten(),
            intent: (self.tab == Tab::Intent)
                .then(|| self.intent.clone())
                .flatten(),
            active_commit: self.active_commit,
            target: self.tab,
            base: 0,
        }
    }

    /// Ob Esc jetzt zum Ursprung des letzten Sprungs führt: nichts davor zu
    /// schließen oder zu leeren, noch im Ziel des Sprungs, auf seiner Ebene.
    fn at_jump_base(&self) -> bool {
        let Some(entry) = self.history.last() else {
            return false;
        };
        if self.help
            || self.tamper.is_some()
            || self.searching
            || !self.query.is_empty()
            || self.tab_filter().is_some_and(|f| !f.is_empty())
            || entry.target != self.tab
        {
            return false;
        }
        match self.tab {
            Tab::Sessions => self.views.len() <= entry.base,
            Tab::Overview => self
                .overview
                .as_ref()
                .is_none_or(|s| s.focus == crate::overview::OverviewFocus::Graph),
            Tab::Changes => self
                .changes
                .as_ref()
                .is_none_or(|s| s.focus == Focus::Files),
            Tab::Verify | Tab::Intent => true,
        }
    }

    /// Kehrt zum Ursprung des letzten Sprungs zurück.
    fn go_back(&mut self) {
        let Some(entry) = self.history.pop() else {
            return;
        };
        self.tab = entry.tab;
        match entry.tab {
            Tab::Sessions => {
                self.query = entry.query;
                self.refilter();
                self.cursor = entry.cursor.min(self.visible.len().saturating_sub(1));
                self.views = entry.views;
            }
            Tab::Overview => {
                if entry.overview.is_some() {
                    self.overview = entry.overview;
                }
            }
            Tab::Verify => {
                if entry.verify.is_some() {
                    self.verify = entry.verify;
                }
            }
            Tab::Changes => {
                if entry.changes.is_some() {
                    self.changes = entry.changes;
                }
            }
            Tab::Intent => {
                if entry.intent.is_some() {
                    self.intent = entry.intent;
                }
            }
        }
        self.active_commit = entry.active_commit.or(self.active_commit);
    }

    /// Der Pfad für den Breadcrumb: je Sprung der Ursprung, dann der
    /// aktuelle Ort — derselbe Ort nie zweimal hintereinander.
    pub fn breadcrumb(&self) -> Vec<String> {
        let shown = match self.tab {
            Tab::Verify => self.verify.as_ref().and_then(|s| s.commit()),
            Tab::Changes => self.changes.as_ref().and_then(|s| s.commit()),
            _ => self.active_commit,
        };
        let mut out: Vec<String> = Vec::new();
        let mut last: Option<Vec<String>> = None;
        for next in self
            .history
            .iter()
            .map(NavEntry::place)
            .chain(std::iter::once(place(self.tab, &self.views, shown)))
        {
            if last.as_ref() != Some(&next) {
                out.extend(next.iter().cloned());
                last = Some(next);
            }
        }
        out
    }

    fn reduce_action(&mut self, action: Action) {
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
            // Die Hilfe scrollt; schließen wie bisher mit ?, Esc oder Enter.
            let page = self.page.max(1) as u16;
            match action {
                Action::Help | Action::Back | Action::Enter => {
                    self.help = false;
                    self.help_scroll = 0;
                }
                Action::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                Action::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                Action::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
                Action::PageDown => self.help_scroll = self.help_scroll.saturating_add(page),
                Action::Home => self.help_scroll = 0,
                Action::End => self.help_scroll = u16::MAX,
                _ => {}
            }
            return;
        }
        // Das Banner zuerst: Enter öffnet die erste manipulierte Session,
        // Esc schließt es. Alle anderen Tasten wirken wie sonst.
        if let Some(alert) = &self.tamper {
            match action {
                Action::Enter => {
                    let first = alert.sessions.first().copied();
                    self.tamper = None;
                    if let Some(id) = first {
                        self.tab = Tab::Sessions;
                        self.searching = false;
                        self.query.clear();
                        self.refilter_keeping(Some(id));
                        self.views.clear();
                        self.push_evidence(id);
                    }
                    return;
                }
                Action::Back => {
                    self.tamper = None;
                    return;
                }
                _ => {}
            }
        }
        // `/` außerhalb der Sessions (B4): derselbe Suchmodus, sein Filter
        // gehört dem Tab.
        if self.tab != Tab::Sessions {
            if self.searching {
                match action {
                    Action::SearchInput(c) => {
                        if let Some(filter) = self.tab_filter_mut() {
                            filter.push(c);
                        }
                        self.refit_tab();
                    }
                    Action::SearchBackspace => {
                        if let Some(filter) = self.tab_filter_mut() {
                            filter.pop();
                        }
                        self.refit_tab();
                    }
                    Action::SearchCommit => self.searching = false,
                    Action::Back => {
                        if let Some(filter) = self.tab_filter_mut() {
                            filter.clear();
                        }
                        self.searching = false;
                        self.refit_tab();
                    }
                    Action::Up | Action::Down => self.step_tab(&action),
                    _ => {}
                }
                return;
            }
            if action == Action::SearchStart {
                if self.tab_filter_mut().is_some() {
                    self.searching = true;
                } else {
                    self.closed = Some("/: nothing to filter here yet".into());
                }
                return;
            }
            // Esc, Schritt 2 (B1): erst den Filter leeren.
            if action == Action::Back && self.tab_filter().is_some_and(|f| !f.is_empty()) {
                if let Some(filter) = self.tab_filter_mut() {
                    filter.clear();
                }
                self.refit_tab();
                return;
            }
            // Danach der Agent-Filter der Übersicht (C3.3) — vor der Ebene.
            if action == Action::Back
                && self.tab == Tab::Overview
                && let Some(state) = self.overview.as_mut()
                && state.agent_only
            {
                state.toggle_agent_only();
                return;
            }
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
        // `z` / `Z` (B4): ein Schritt feiner bzw. gröber, an den Enden bleibt
        // die Stufe.
        let action = match action {
            Action::ZoomStep(finer) => {
                let d = self.zoom.digit();
                Action::Zoom(if finer {
                    (d + 1).min(3)
                } else {
                    d.saturating_sub(1).max(1)
                })
            }
            other => other,
        };
        if let Action::Zoom(d) = action {
            self.zoom = self.zoom.from_digit(d);
            self.relayout();
            return;
        }
        if action == Action::Help {
            self.help = true;
            return;
        }
        // Die Objekt-Verben (B4): `v d i` überall, `w e` dort, wo der Tab
        // sie nicht selbst trägt.
        let verb = match (action.clone(), self.tab) {
            (Action::Verify | Action::Diff | Action::Intent, _) => true,
            (Action::Why, Tab::Verify | Tab::Changes | Tab::Intent) => true,
            (Action::Evidence, Tab::Overview | Tab::Verify | Tab::Changes | Tab::Intent) => true,
            (Action::Context, tab) => tab != Tab::Changes,
            _ => false,
        };
        if verb {
            self.verb(action);
            return;
        }
        // `a` (C3.3): nur in der Übersicht — anderswo ein Hinweis statt
        // still nichts.
        if action == Action::AgentFilter {
            match self.overview.as_mut().filter(|_| self.tab == Tab::Overview) {
                Some(state) => state.toggle_agent_only(),
                None => {
                    self.closed =
                        Some("a: shows only commits with an agent session — in Overview (1)".into())
                }
            }
            return;
        }
        match action {
            Action::CycleTab(forward) => {
                let n = Tab::ALL.len();
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.show_tab(
                    Tab::ALL[if forward {
                        (i + 1) % n
                    } else {
                        (i + n - 1) % n
                    }],
                );
                return;
            }
            Action::TabTo(i) => {
                if let Some(tab) = Tab::ALL.get(usize::from(i)) {
                    self.show_tab(*tab);
                }
                return;
            }
            _ => {}
        }
        if self.tab == Tab::Changes {
            self.reduce_changes(action);
            return;
        }
        if self.tab == Tab::Verify {
            self.reduce_verify(action);
            return;
        }
        if self.tab == Tab::Intent {
            self.reduce_intent(action);
            return;
        }
        if self.tab == Tab::Overview {
            self.reduce_overview(action);
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
                    // Baum ↔ Zeitleiste: derselbe Knoten bleibt fokussiert (B5).
                    let node = rows.get(cursor).map(|r| r.node);
                    let rows = if timeline {
                        layout::timeline(&graph, self.zoom)
                    } else {
                        layout::rows(&graph, self.zoom)
                    };
                    let cursor = node
                        .and_then(|n| layout::locate(&graph, self.zoom, &rows, n))
                        .unwrap_or_else(|| cursor.min(rows.len().saturating_sub(1)));
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
                        // Der Anker der Absicht: im Intent-Tab — der Stapel
                        // bleibt, Tab führt zurück.
                        Some(WhyStep::Intent {
                            anchor: Some(anchor),
                            ..
                        }) => {
                            let id = anchor.id.clone();
                            self.views.push(View::Why {
                                origin,
                                chain,
                                cursor,
                                edge,
                                inspector,
                            });
                            self.open_intent(Some(id));
                            return;
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
                let mut jump = None;
                match action {
                    Action::Up => cursor = cursor.saturating_sub(1),
                    Action::Down => cursor = (cursor + 1).min(last),
                    Action::Home | Action::PageUp => cursor = 0,
                    Action::End | Action::PageDown => cursor = last,
                    Action::Back => keep = false,
                    // Enter auf ARTIFACT: der Diff des abgeglichenen Commits.
                    Action::Enter if report.is_some() && cursor == ARTIFACT_SECTION => {
                        jump = artifacts.first().map(|a| a.commit);
                    }
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
                if let Some(commit) = jump {
                    self.open_changes(Some(commit));
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
            // Esc (B1): die Suche leeren — sonst nichts. Esc beendet nie;
            // das tun nur `q` und Ctrl-C.
            Action::Back if !self.query.is_empty() => {
                self.query.clear();
                self.refilter();
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
        let mut verified = Instant::now()
            .checked_sub(VERIFY_EVERY)
            .unwrap_or_else(Instant::now);
        let mut anchored = verified;
        // Prüfungen im Worker, wenn die Quelle das anbietet (B6) — sonst
        // synchron wie bisher.
        let mut worker = source.background().map(Worker::spawn);
        while !self.quit {
            self.now = SystemTime::now();
            terminal.draw(|frame| view::draw(frame, &mut self))?;
            // Steht eine Prüfung der Signer aus, ist „checking…" jetzt
            // gezeichnet: erst dann prüfen (ssh-keygen je Seal) und sofort
            // neu zeichnen.
            // Vom Nutzer verlangt: sofort; nach einem Neuladen gedrosselt —
            // jede Prüfung startet ssh-keygen je Seal und `minds verify`.
            let due = self
                .verify
                .as_ref()
                .is_some_and(|v| v.due(verified.elapsed(), VERIFY_EVERY));
            if self.tab == Tab::Verify && due {
                match worker.as_mut() {
                    // Im Worker (B6): die Schleife bleibt bei den Tasten.
                    Some(worker) => {
                        if self.send_check(worker) {
                            verified = Instant::now();
                        }
                    }
                    None => {
                        self.fill_verify(source);
                        verified = Instant::now();
                        continue;
                    }
                }
            }
            // Das Detail der Übersicht (C3.4): das Urteil des gewählten
            // Commits, gedrosselt wie der Verify-Tab.
            let due = self.tab == Tab::Overview
                && self
                    .overview
                    .as_ref()
                    .is_some_and(|o| o.check_due().is_some())
                && verified.elapsed() >= VERIFY_EVERY;
            if due {
                match worker.as_mut() {
                    Some(worker) => {
                        if self.send_overview_check(worker) {
                            verified = Instant::now();
                        }
                    }
                    None => {
                        self.fill_overview_verify(source);
                        verified = Instant::now();
                        continue;
                    }
                }
            }
            if let Some(worker) = worker.as_mut() {
                if self.take_checks(worker) {
                    continue;
                }
                if worker.busy.is_some() {
                    self.spin = self.spin.wrapping_add(1);
                }
            }
            let due = self
                .intent
                .as_ref()
                .is_some_and(|i| i.due(anchored.elapsed(), VERIFY_EVERY));
            if self.tab == Tab::Intent && due {
                self.fill_intents(source);
                anchored = Instant::now();
                continue;
            }
            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()?
                    && key.kind != event::KeyEventKind::Release
                {
                    let action = input::map(key, self.searching);
                    self.reduce(action);
                }
            } else if self.fill_overview(source) {
                // Im Leerlauf: Anker und Zeilen des gewählten Commits (C3.4).
                continue;
            } else if self.fill_conformance(source) {
                // Im Leerlauf, nie beim Scrollen: je Tick höchstens ein
                // Commit, danach sofort neu zeichnen.
                continue;
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

/// Was den Abgleich eines Commits bestimmt, ohne I/O: die Sessions, die ihn
/// tragen, und die, die sein Trailer nennt (zusammen die Claimants).
fn commit_inputs(inspection: &Inspection, commit: CommitId) -> (Vec<SessionId>, Vec<SessionId>) {
    let index = inspection.index();
    (
        index.sessions_of(commit).to_vec(),
        index.trailer_ids(commit).to_vec(),
    )
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

/// Ein Ort für den Breadcrumb: der Tab mit seinem Gegenstand — im
/// Sessions-Tab die Ebenen.
fn place(tab: Tab, views: &[View], commit: Option<CommitId>) -> Vec<String> {
    let short_commit = |c: CommitId| c.to_string().chars().take(7).collect::<String>();
    match tab {
        Tab::Sessions if !views.is_empty() => views
            .iter()
            .map(|view| match view {
                View::Graph { id, .. } => format!("Session {}…", short(*id)),
                View::Why { .. } => "Why".to_string(),
                View::Evidence { .. } => "Evidence".to_string(),
            })
            .collect(),
        Tab::Overview | Tab::Verify | Tab::Changes => {
            let mut out = vec![tab.title().to_string()];
            out.extend(commit.map(short_commit));
            out
        }
        _ => vec![tab.title().to_string()],
    }
}
