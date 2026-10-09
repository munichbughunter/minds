//! Der Intent-Tab: die Anker unter `refs/minds/intents/` — Quelle, Inhalt,
//! Bereich, Beleg, Signatur, Snapshot — und welche Sessions sich an sie
//! gebunden nennen.
//!
//! Strikt lesend. Die Quelle liefert die Anker geprüft wie `minds intent
//! show` (Beleg) und `minds verify` (Signatur gegen die vertrauenswürdigen
//! Signer); die Bindungen der Sessions liest der Tab selbst aus dem Index —
//! als Record, ungeprüft ([`minds_reader::intent::bound_anchor`]).

use std::time::Duration;

use minds_core::{ContentHash, SessionId};
use minds_reader::Inspection;
use minds_reader::intent::{AnchorRecord, bound_anchor};

use crate::{IntentDetail, IntentInfo, IntentList};

/// Der Zustand des Tabs.
#[derive(Debug, Clone)]
pub struct IntentTab {
    /// Die Anker — `None`, solange die Quelle noch nicht gefragt ist;
    /// `Err` mit Grund, wenn sie nicht lesen konnte.
    pub entries: Option<Result<Vec<IntentInfo>, String>>,
    /// Wie viele Anker der Store hat (mehr als gelesen: gekappt).
    pub total: usize,
    /// Wie viele gelistete das Byte-Budget übersprang.
    pub skipped: usize,
    /// Die Cursorzeile.
    pub cursor: usize,
    /// Die erste gezeigte Zeile des Details (gerenderte, umbrochene Zeilen).
    pub scroll: u16,
    /// Wie weit das Detail höchstens blättert — setzt das Zeichnen, das die
    /// umbrochenen Zeilen kennt.
    pub max_scroll: std::cell::Cell<u16>,
    /// Ob die Quelle (wieder) gefragt werden muss.
    pub pending: bool,
    /// Ob der Nutzer das verlangt hat — dann ungedrosselt.
    pub urgent: bool,
    /// Ob der Stand seit dem letzten Holen neu geladen wurde: Dann gilt
    /// kein „valid" und kein „proven" mehr, bis neu geprüft ist.
    pub stale: bool,
    /// Der Anker, auf den nach dem nächsten Holen gesprungen wird.
    pub want: Option<ContentHash>,
}

impl IntentTab {
    /// Ein frischer Tab, der sofort fragt und dann auf `want` springt.
    pub fn new(want: Option<ContentHash>) -> Self {
        Self {
            entries: None,
            total: 0,
            skipped: 0,
            cursor: 0,
            scroll: 0,
            max_scroll: std::cell::Cell::new(0),
            pending: true,
            urgent: true,
            stale: false,
            want,
        }
    }

    /// Ob die Quelle jetzt gefragt werden soll: ausstehend, und vom Nutzer
    /// verlangt oder seit `every` nicht mehr gefragt.
    pub fn due(&self, since: Duration, every: Duration) -> bool {
        self.pending && (self.urgent || since >= every)
    }

    /// Die Anker, soweit gelesen.
    pub fn list(&self) -> &[IntentInfo] {
        match &self.entries {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// Der Anker unter dem Cursor.
    pub fn selected(&self) -> Option<&IntentInfo> {
        self.list().get(self.cursor)
    }

    /// Springt auf `id` — jetzt, oder nach dem nächsten Holen.
    pub fn select(&mut self, id: &ContentHash) {
        match self.list().iter().position(|i| &i.id == id) {
            Some(at) => {
                self.cursor = at;
                self.scroll = 0;
            }
            None => self.want = Some(id.clone()),
        }
    }

    /// Nach einem Neuladen: neu fragen, bis dahin nichts als geprüft zeigen.
    pub fn invalidate(&mut self, urgent: bool) {
        self.urgent = urgent || (self.pending && self.urgent);
        self.pending = true;
        self.stale = self.entries.is_some();
    }

    /// Übernimmt die Antwort der Quelle — derselbe Anker bleibt gewählt,
    /// ein verlangter (`want`) geht vor.
    pub fn fill(&mut self, answer: Result<IntentList, String>) {
        let keep = self.selected().map(|i| i.id.clone());
        let before = self.cursor;
        match answer {
            Ok(list) => {
                self.total = list.total;
                self.skipped = list.skipped;
                self.entries = Some(Ok(list.entries));
            }
            Err(why) => self.entries = Some(Err(why)),
        }
        self.pending = false;
        self.urgent = false;
        self.stale = false;
        for id in [self.want.take(), keep].into_iter().flatten() {
            if let Some(at) = self.list().iter().position(|i| i.id == id) {
                self.cursor = at;
                break;
            }
        }
        self.cursor = self.cursor.min(self.list().len().saturating_sub(1));
        if self.cursor != before {
            self.scroll = 0;
        }
    }
}

/// Die Anker, die Sessions des Stands nennen — für die Quelle, die sie
/// immer liest.
pub fn named_anchors(inspection: &Inspection) -> Vec<ContentHash> {
    let mut named: Vec<ContentHash> = inspection
        .index()
        .sessions()
        .filter_map(|(_, session)| bound_anchor(session).map(|a| a.id))
        .collect();
    named.sort();
    named.dedup();
    named
}

/// Die Sessions, die sich an `id` gebunden nennen — mit der Art der
/// Bindung, nach Id.
pub fn bound_sessions(inspection: &Inspection, id: &ContentHash) -> Vec<(SessionId, AnchorRecord)> {
    inspection
        .index()
        .sessions()
        .filter_map(|(sid, session)| {
            bound_anchor(session)
                .filter(|a| &a.id == id)
                .map(|a| (*sid, a.record))
        })
        .collect()
}

/// Der gelesene Anker, falls lesbar.
pub fn detail(info: &IntentInfo) -> Option<&IntentDetail> {
    info.detail.as_ref().ok()
}
