//! Der Decisions-Tab: die Entscheidungen unter `refs/minds/decisions/` mit
//! ihrem Stand heute — offen, neu zu bewerten, anzusehen, ungeprüft, in
//! Kraft — und welche Sessions sie aufgenommen haben.
//!
//! Strikt lesend. Die Quelle liefert die Entscheidungen bewertet wie
//! `minds decisions` (Proben gegen den Index, zur Lesezeit, nie
//! gespeichert); der Tab zeigt die Spitzen der Ketten, abgelöste Records
//! als Historie ihrer Nachfolgerin. Jeder Text stammt aus dem entschärften
//! [`Assessment`].

use std::time::Duration;

use minds_core::{ContentHash, SessionId};
use minds_reader::Inspection;
use minds_reader::decisions::{Assessment, Standing};

use crate::DecisionList;

/// Der Zustand des Tabs.
#[derive(Debug, Clone)]
pub struct DecisionTab {
    /// Die Bewertungen — `None`, solange die Quelle noch nicht gefragt ist;
    /// `Err` mit Grund, wenn sie nicht lesen konnte. Sortiert wie
    /// `assess_all`: Spitzen zuerst, nach Dringlichkeit, dann die jüngste.
    pub entries: Option<Result<Vec<Assessment>, String>>,
    /// Records, die sich nicht lesen ließen.
    pub unreadable: usize,
    /// Ob nicht jeder Record gelesen wurde.
    pub truncated: bool,
    /// Die Cursorzeile — ein Index in [`heads`](Self::heads).
    pub cursor: usize,
    /// Die erste gezeigte Zeile des Details.
    pub scroll: u16,
    /// Wie weit das Detail höchstens blättert — setzt das Zeichnen.
    pub max_scroll: std::cell::Cell<u16>,
    /// Ob die Quelle (wieder) gefragt werden muss.
    pub pending: bool,
    /// Ob der Nutzer das verlangt hat — dann ungedrosselt.
    pub urgent: bool,
    /// Ob der Stand seit dem letzten Holen neu geladen wurde: Dann gilt
    /// kein Probenergebnis mehr, bis neu gerechnet ist.
    pub stale: bool,
    /// Die Entscheidung, auf die nach dem nächsten Holen gesprungen wird.
    pub want: Option<ContentHash>,
    /// Der Filter aus `/` (B4) — leer: alle.
    pub filter: String,
    /// Ob der TECHNICAL-Block (Proben im Wortlaut, Ids) aufgeklappt ist.
    pub technical_open: bool,
}

impl DecisionTab {
    /// Ein frischer Tab, der sofort fragt und dann auf `want` springt.
    pub fn new(want: Option<ContentHash>) -> Self {
        Self {
            entries: None,
            unreadable: 0,
            truncated: false,
            cursor: 0,
            scroll: 0,
            max_scroll: std::cell::Cell::new(0),
            pending: true,
            urgent: true,
            stale: false,
            want,
            filter: String::new(),
            technical_open: false,
        }
    }

    /// Ob die Quelle jetzt gefragt werden soll.
    pub fn due(&self, since: Duration, every: Duration) -> bool {
        self.pending && (self.urgent || since >= every)
    }

    /// Alle Bewertungen, soweit gelesen — Spitzen und abgelöste.
    pub fn all(&self) -> &[Assessment] {
        match &self.entries {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// Die Spitzen der Ketten, in Quell-Reihenfolge — die Liste des Tabs.
    pub fn heads(&self) -> Vec<&Assessment> {
        self.all().iter().filter(|a| a.is_head()).collect()
    }

    /// Die Spitzen, die der Filter aus `/` durchlässt (B4): Id, Thema,
    /// Stand, Satz.
    pub fn shown(&self) -> Vec<usize> {
        let heads = self.heads();
        crate::filter::shown_by(heads.len(), &self.filter, |i| {
            let a = heads[i];
            format!(
                "{} {} {} {}",
                a.id,
                a.topic,
                a.standing.as_str(),
                a.headline()
            )
        })
    }

    /// Die Entscheidung unter dem Cursor.
    pub fn selected(&self) -> Option<&Assessment> {
        self.heads().get(self.cursor).copied()
    }

    /// Die Bewertung zu einer Id — Spitze oder abgelöst.
    pub fn by_id(&self, id: &ContentHash) -> Option<&Assessment> {
        self.all().iter().find(|a| &a.id == id)
    }

    /// Springt auf `id` — jetzt, oder nach dem nächsten Holen. Eine
    /// abgelöste Entscheidung öffnet ihre Nachfolgerin. `false`, wenn die
    /// Entscheidung **bekannt** fehlt: frisch geholt, und nicht dabei —
    /// dann wird nicht neu geholt (ein `D` auf eine Id, die der Store
    /// nicht hat, darf nicht jedes Mal alle Proben laufen lassen).
    pub fn select(&mut self, id: &ContentHash) -> bool {
        let target = self
            .by_id(id)
            .and_then(|a| a.superseded_by.clone())
            .map(|successor| self.head_of(&successor))
            .unwrap_or_else(|| id.clone());
        match self.heads().iter().position(|a| a.id == target) {
            Some(at) => {
                self.cursor = at;
                self.scroll = 0;
                true
            }
            None if matches!(self.entries, Some(Ok(_))) && !self.stale && !self.pending => false,
            // Noch nicht (oder nicht mehr) geholt: nach dem nächsten Holen —
            // und das jetzt, sonst ginge der Sprung bis zum gedrosselten
            // Holen verloren.
            None => {
                self.want = Some(id.clone());
                self.pending = true;
                self.urgent = true;
                true
            }
        }
    }

    /// Die Spitze der Kette, in der `id` steht.
    fn head_of(&self, id: &ContentHash) -> ContentHash {
        let mut at = id.clone();
        for _ in 0..self.all().len() {
            match self.by_id(&at).and_then(|a| a.superseded_by.clone()) {
                Some(next) => at = next,
                None => break,
            }
        }
        at
    }

    /// Space klappt TECHNICAL auf oder zu.
    pub fn toggle_technical(&mut self) {
        self.technical_open = !self.technical_open;
    }

    /// Nach einem Neuladen: neu fragen, bis dahin nichts als geprüft zeigen.
    pub fn invalidate(&mut self, urgent: bool) {
        self.urgent = urgent || (self.pending && self.urgent);
        self.pending = true;
        self.stale = self.entries.is_some();
    }

    /// Übernimmt die Antwort der Quelle — dieselbe Entscheidung bleibt
    /// gewählt, eine verlangte (`want`) geht vor.
    pub fn fill(&mut self, answer: Result<DecisionList, String>) {
        let keep = self.selected().map(|a| a.id.clone());
        match answer {
            Ok(list) => {
                self.unreadable = list.unreadable;
                self.truncated = list.truncated;
                self.entries = Some(Ok(list.entries));
            }
            Err(why) => self.entries = Some(Err(why)),
        }
        self.pending = false;
        self.urgent = false;
        self.stale = false;
        // Ein verlangter Record kann abgelöst sein: dann seine Nachfolgerin.
        let want = self.want.take().map(|id| self.head_of(&id));
        let before = self.cursor;
        for id in [want, keep].into_iter().flatten() {
            if let Some(at) = self.heads().iter().position(|a| a.id == id) {
                self.cursor = at;
                break;
            }
        }
        self.cursor = self.cursor.min(self.heads().len().saturating_sub(1));
        if self.cursor != before {
            self.scroll = 0;
            self.technical_open = false;
        }
    }

    /// Wie viele Spitzen je Stand.
    pub fn count(&self, standing: Standing) -> usize {
        self.heads()
            .iter()
            .filter(|a| a.standing == standing)
            .count()
    }
}

/// Die Sessions, die `id` aufgenommen haben (`decision_events`), in
/// Index-Reihenfolge — ein Record, kein Beleg.
pub fn naming_sessions(inspection: &Inspection, id: &ContentHash) -> Vec<SessionId> {
    inspection
        .cards()
        .into_iter()
        .filter(|card| {
            inspection
                .index()
                .session(card.id)
                .is_some_and(|s| s.decision_events.iter().any(|e| &e.decision_id == id))
        })
        .map(|card| card.id)
        .collect()
}

/// Die Entscheidungen, die eine Session aufgenommen hat, in
/// Journal-Reihenfolge.
pub fn named_decisions(inspection: &Inspection, session: SessionId) -> Vec<ContentHash> {
    inspection
        .index()
        .session(session)
        .map(|s| {
            s.decision_events
                .iter()
                .map(|e| e.decision_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Ob eine Nennung zu ihrem Aufruf passt: Der Record entstand im Zeitfenster
/// des Aufrufs (`minds_reader::decisions::named_within_call`). Solange die
/// Records nicht geholt sind, gilt die Nennung als noch nicht prüfbar und
/// bleibt stehen („… reading"); eine Id, die nach vollständigem Lesen kein
/// Record ist, fällt weg ([`unknown_counts`]).
fn event_fits(
    session: &minds_core::Session,
    event: &minds_core::decision::DecisionEvent,
    tab: Option<&DecisionTab>,
) -> bool {
    // Ohne Zug und Aufruf ist die Nennung nie prüfbar — nicht erst, wenn
    // der Record da ist.
    if event.turn.is_none() || event.call.is_none() {
        return false;
    }
    match tab.and_then(|t| t.by_id(&event.decision_id)) {
        Some(a) => minds_reader::decisions::named_within_call(session, event, &a.record.at),
        None => unknown_counts(tab),
    }
}

/// Ob eine genannte Id ohne Record noch zählt: solange die Records nicht
/// geholt oder veraltet sind, ja — „… reading". Danach ist eine Id, die
/// kein gelesener Record ist, keine Entscheidung dieser Session: Ein
/// Kommando neben `minds decide` hat sie gedruckt — oder der Store ist
/// über dem Deckel, und auch dann gilt nur, was gelesen ist (ein Agent
/// kann den Store über den Deckel schreiben). Die CLI (`recall`) lässt
/// sie ebenso weg.
pub(crate) fn unknown_counts(tab: Option<&DecisionTab>) -> bool {
    match tab {
        Some(t) => t.stale || !matches!(t.entries, Some(Ok(_))),
        None => true,
    }
}

/// [`named_decisions`], ohne die Nennungen, die nicht zu ihrem Aufruf passen
/// (eine fremde Id, die ein Kommando neben `minds decide` druckte).
pub fn linked_decisions(
    inspection: &Inspection,
    tab: Option<&DecisionTab>,
    session: SessionId,
) -> Vec<ContentHash> {
    let Some(s) = inspection.index().session(session) else {
        return Vec::new();
    };
    let mut out: Vec<ContentHash> = Vec::new();
    for event in s.decision_events.iter().filter(|e| event_fits(s, e, tab)) {
        if !out.contains(&event.decision_id) {
            out.push(event.decision_id.clone());
        }
    }
    out
}

/// [`naming_sessions`], nur mit Sessions, deren Nennung zu ihrem Aufruf passt.
pub fn sessions_naming(
    inspection: &Inspection,
    tab: Option<&DecisionTab>,
    id: &ContentHash,
) -> Vec<SessionId> {
    naming_sessions(inspection, id)
        .into_iter()
        .filter(|sid| {
            inspection.index().session(*sid).is_some_and(|s| {
                s.decision_events
                    .iter()
                    .any(|e| &e.decision_id == id && event_fits(s, e, tab))
            })
        })
        .collect()
}
