//! An welche Anforderung ist eine Session gebunden (EA-14)? Zur Lesezeit
//! aus gespeichertem Material berechnet, nie gespeichert (W2/W5).
//!
//! # Die Eingänge
//!
//! - Die **Epochen-Kette** der Session ([`EpochChain`]): Epochen sind eigene
//!   Sessions (ADR-0011), und das Intent-Event, mit dem die Kette beginnt,
//!   steht in der **ersten**. Wer nur die geprüfte Session ansieht, sähe ab
//!   der zweiten Epoche „nicht gebunden". [`epoch_chain`] sammelt die
//!   Vorgänger — nur über Seals, die der Aufrufer geprüft hat.
//! - Der Store: Anker, Snapshot und Signatur unter `refs/minds/intents/`.
//! - Eine Signaturprüfung des Aufrufers: Der Reader kennt weder Schlüssel
//!   noch `ssh-keygen`; `minds verify` prüft unter `minds-intent` gegen
//!   `--signers` (EA-15).
//!
//! # Die Regeln
//!
//! - Verkettet (`chained`) sind nur `minds.intent`-Events aus Sessions, die
//!   über geprüfte Seals erreicht wurden ([`EpochChain::witnessed`]). Sonst
//!   zählt der Anker als nicht verkettet — gespeicherte Felder allein kann
//!   jeder schreiben, der `refs/minds/` schreibt.
//! - Verkettete Events gehen vor der lokalen Datei-Bindung (`intent_anchor`,
//!   A1).
//! - Gebunden ist der Anker, der **am Ende** des geprüften Materials galt.
//! - `from_session_start` nur mit **positivem** Beleg: Das erste
//!   Intent-Event der Kette trägt `opens_session` — der Witness schreibt es
//!   ausschließlich vor dem ersten Hook-Event einer neuen Session. Ein Seal
//!   ohne `previous` ist kein Beleg (dort heißt `None` nur „nicht belegt").
//!   Bricht der Weg zurück vorher ab, fehlt der Beleg: fail-closed.
//! - `changed_mid_session`: Ein späteres Intent-Event nennt einen anderen
//!   Anker als das erste.
//! - `snapshot_matches`: Anker hash-geprüft im Store, und der abgelegte
//!   Snapshot hasht auf `content=`. Fehlt etwas davon: `false`.

use std::collections::BTreeSet;

use minds_core::evidence::{SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::intent_anchor::{IntentEvent, content_hash};
use minds_core::{ContentHash, Session, SessionId};
use minds_store::ContextStore;

use crate::assurance::{IntentSignature, IntentState};

/// So viele Epochen hält [`epoch_chain`] höchstens.
pub const MAX_EPOCHS: usize = 4096;

/// Die Sessions einer Epochen-Kette, älteste zuerst, die geprüfte zuletzt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochChain {
    /// Die Sessions, älteste zuerst.
    pub sessions: Vec<Session>,
    /// Jede Session wurde über einen vom Aufrufer geprüften Seal erreicht,
    /// der sie nennt. `false`: Ihre Intent-Events sind Behauptungen.
    pub witnessed: bool,
}

/// Die Signaturprüfung des Aufrufers: `(Ankertext, armierte Signatur)`.
pub type CheckSignature<'a> = dyn Fn(&str, &str) -> IntentSignature + 'a;

/// Die Seal-Prüfung des Aufrufers: `(seal_id, Seal)` ist ein gültig
/// signierter **Witness**-Seal — Scope `witness/v1` **und** Signatur unter
/// dem Namespace `minds-witness` gegen vertrauenswürdige Signer (EA-12).
/// Eine Prüfung nur „irgendeine gültige Signatur" ließe einen lokal
/// signierten `agent-hooks/v1`-Seal als bezeugt durchgehen. Dieser Vertrag
/// trägt `EpochChain::witnessed`; EA-15 testet ihn mit einem solchen Seal.
pub type TrustSeal<'a> = dyn Fn(&ContentHash, &Seal) -> bool + 'a;

/// Die Intent-Bindung der letzten Session in `chain`.
pub fn intent_of(
    chain: &EpochChain,
    store: &dyn ContextStore,
    check: &CheckSignature<'_>,
) -> IntentState {
    let Some(last) = chain.sessions.last() else {
        return IntentState::Unbound;
    };
    let events: Vec<&IntentEvent> = chain
        .sessions
        .iter()
        .flat_map(|session| session.intent_events.iter())
        .collect();
    let (anchor_id, chained, from_session_start, changed_mid_session) =
        match (events.first(), events.last()) {
            (Some(first), Some(latest)) if chain.witnessed => (
                latest.anchor_id.clone(),
                true,
                first.opens_session,
                events.iter().any(|e| e.anchor_id != first.anchor_id),
            ),
            // Nicht über geprüfte Seals erreicht: eine Behauptung, keine
            // Verkettung.
            (Some(_), Some(latest)) => (latest.anchor_id.clone(), false, false, false),
            _ => match &last.intent_anchor {
                Some(id) => (id.clone(), false, false, false),
                None => return IntentState::Unbound,
            },
        };
    let (signature, snapshot_matches) = material(store, &anchor_id, check);
    IntentState::Bound {
        anchor_id,
        chained,
        signature,
        snapshot_matches,
        from_session_start,
        changed_mid_session,
    }
}

/// Signaturlage und Snapshot-Abgleich eines Ankers aus dem Store.
fn material(
    store: &dyn ContextStore,
    id: &ContentHash,
    check: &CheckSignature<'_>,
) -> (IntentSignature, bool) {
    match store.get_intent(id) {
        Ok(Some(stored)) => {
            let matches = content_hash(&stored.snapshot) == stored.anchor.content;
            let signature = match store.intent_signature(id) {
                Ok(Some(signature)) => check(&stored.text, &signature),
                Ok(None) => IntentSignature::Unsigned,
                // Eine abgelegte Signatur ohne `ssh-sig`-Form gilt unter
                // keinem Namespace (EA-15) — sonst machte eine kaputte
                // `anchor.sig` aus „ungültig" ein „nicht geprüft".
                Err(minds_store::StoreError::IntentSignatureMalformed { .. }) => {
                    IntentSignature::Invalid
                }
                // Eine Signatur liegt vielleicht vor, ist aber nicht lesbar.
                Err(_) => IntentSignature::NotChecked,
            };
            (signature, matches)
        }
        // Kein Anker im Store (etwa noch nicht gesynct): Ob er signiert ist,
        // ist unbekannt — nicht „unsigniert".
        Ok(None) => (IntentSignature::NotChecked, false),
        // Verändert oder unlesbar: nichts davon wird geprüft.
        Err(_) => (IntentSignature::NotChecked, false),
    }
}

/// Die Epochen-Kette bis einschließlich `session` (Id `id`), älteste
/// zuerst.
///
/// `seals` sind die Seals, die der Aufrufer der Session zuordnet (etwa
/// `seals_of`). Gezählt werden nur die, die `trusted` bestätigt **und** die
/// genau diese Session nennen; der Rückverweis allein ist beschreibbar.
/// Nennen sie verschiedene Vorgänger, endet der Weg hier. Zurück geht es
/// über `previous`, solange jeder Vorgänger lesbar, hash-geprüft, `trusted`
/// und `stored` ist; alles andere (fehlend, verändert, unsigniert,
/// zurückgewiesen, Zyklus, [`MAX_EPOCHS`]) beendet den Weg. Ein früh
/// endender Weg kostet nur Belege (kein `opens_session` in Sicht), er
/// erfindet keine.
pub fn epoch_chain(
    store: &dyn ContextStore,
    id: SessionId,
    session: Session,
    seals: &[(ContentHash, Seal)],
    trusted: &TrustSeal<'_>,
) -> EpochChain {
    let names = |seal: &Seal, id: SessionId| {
        matches!(&seal.outcome, SealOutcome::Stored { session }
            if session.parse::<SessionId>().ok() == Some(id))
    };
    let own: Vec<&Seal> = seals
        .iter()
        // Der Scope wird hier geprüft, nicht nur in `trusted`: Ein lokal
        // signierter `agent-hooks/v1`-Seal bezeugt nie etwas.
        .filter(|(seal_id, seal)| {
            seal.scope == SCOPE_WITNESS_V1 && names(seal, id) && trusted(seal_id, seal)
        })
        .map(|(_, seal)| seal)
        .collect();
    // Seals und Session gehören zusammen: Die Id wird aus der Session
    // selbst gerechnet, nicht nur vom Aufrufer behauptet.
    let matches = SessionId::of(&session).is_ok_and(|actual| actual == id);
    let mut chain = vec![session];
    if own.is_empty() || !matches {
        return EpochChain {
            sessions: chain,
            witnessed: false,
        };
    }
    let previous: BTreeSet<Option<&ContentHash>> =
        own.iter().map(|seal| seal.previous.as_ref()).collect();
    let mut next = match previous.into_iter().collect::<Vec<_>>().as_slice() {
        [Some(previous)] => Some((*previous).clone()),
        _ => None,
    };
    let mut seen = BTreeSet::new();
    while let Some(previous) = next.take() {
        if !seen.insert(previous.clone()) || chain.len() >= MAX_EPOCHS {
            break;
        }
        let Some(seal) = store
            .seal_text(&previous)
            .ok()
            .flatten()
            .and_then(|text| Seal::parse(&text).ok())
        else {
            break;
        };
        if seal.scope != SCOPE_WITNESS_V1 || !trusted(&previous, &seal) {
            break;
        }
        let SealOutcome::Stored { session } = &seal.outcome else {
            break;
        };
        let Some(earlier) = session
            .parse::<SessionId>()
            .ok()
            .and_then(|id| store.get(id).ok().flatten())
        else {
            break;
        };
        chain.push(earlier);
        next = seal.previous;
    }
    chain.reverse();
    EpochChain {
        sessions: chain,
        witnessed: true,
    }
}
