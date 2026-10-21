//! Die Beobachtungen des Datei-Beobachters (EA-08) als Lese-Modell — und die
//! daraus abgeleitete **Korroboration** von Schreib-Claims (W5: Ableitungen
//! leben im Reader, nichts davon wird gespeichert, W2).
//!
//! # Welche Beobachtungen zählen
//!
//! Unter `refs/minds/observations/` kann im geteilten Repository auch der
//! Agent schreiben. Ein Objekt zählt deshalb nur, wenn ein Seal mit Scope
//! `witness-fs/v1` und Outcome `observations_stored` es nennt **und** der
//! Aufrufer diesen Seal als vertrauenswürdig bestätigt (`trusted`, typisch:
//! gültige `minds-witness`-Signatur gegen `allowed_signers`). Ohne diese
//! Bestätigung gibt es keine Beobachtungen — fail-closed: Eine gefälschte
//! Beobachtung könnte sonst eine menschliche Änderung zu
//! `explained (fs only)` machen.
//!
//! # Abbildung auf die Reconciliation
//!
//! - `content: "b3-…"` → beobachteter Inhalt.
//! - `reason: "deleted"` → beobachtete Abwesenheit.
//! - jeder andere Grund (Secret-Datei, zu groß, Ziel außerhalb, zweiter
//!   harter Link) → die Datei war da, ihr Inhalt ist bewusst unbekannt
//!   ([`FsObservation::opaque`]): Sie bestätigt nichts. Ist sie der jüngste
//!   Stand, zählt eine frühere lesbare Beobachtung, die dem Commit
//!   widerspricht, weiter; sonst zählen die Claims wie ohne Witness.

use jiff::{SignedDuration, Timestamp};
use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome};
use minds_core::observation::{GapReason, ObservationReason};
use minds_core::{ContentHash, EffectKind, Session, SessionId, ToolCall};
use minds_store::ContextStore;

use crate::reconcile::{FsObservation, LineSource, ObservedAt, claim_path};

/// Spielraum um das Fenster der verknüpften Sessions (± 30 s).
pub const WINDOW_SLACK: SignedDuration = SignedDuration::from_secs(30);

/// So lange **vor** dem Claim darf die passende Beobachtung liegen (Uhren
/// von Hook und Witness laufen nicht synchron).
pub const CORROBORATION_BEFORE: SignedDuration = SignedDuration::from_secs(2);

/// So lange **nach** dem Claim darf sie liegen (Entprellung, Latenz).
pub const CORROBORATION_AFTER: SignedDuration = SignedDuration::from_secs(30);

/// So lange **vor** dem Pre-Event eines Shell-Aufrufs darf eine Beobachtung
/// liegen, die noch zu ihm zählt (Uhren von Hook und Witness).
pub const EXEC_WINDOW_BEFORE: SignedDuration = CORROBORATION_BEFORE;

/// Der Latenzpuffer des Beobachters **nach** dem Post-Event eines
/// Shell-Aufrufs: Er wartet je Pfad Ruhe ab (Entprellung), liest dann und
/// stempelt beim Lesen. Bewusst enger als [`CORROBORATION_AFTER`]: Ein
/// Schreib-Claim bindet seine Beobachtung zusätzlich über den Hash, das
/// Fenster eines Shell-Aufrufs nur über die Zeit.
pub const EXEC_WINDOW_AFTER: SignedDuration = SignedDuration::from_secs(5);

/// Das Zeitfenster eines Shell-Aufrufs (Exec): `[PreToolUse −
/// EXEC_WINDOW_BEFORE, PostToolUse + EXEC_WINDOW_AFTER]`. Eine Beobachtung
/// des Datei-Beobachters in diesem Fenster wird dem Aufruf zugeordnet
/// ([`exec_call_at`]): Was die Shell schrieb (Heredoc, `tee`, `sed -i`, ein
/// Generator), hat keinen Tool-Payload und damit keinen Schreib-Claim — der
/// Witness sah es trotzdem, und zwar während dieses Aufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecWindow {
    pub source: LineSource,
    pub from: Timestamp,
    pub to: Timestamp,
}

/// Das Fenster **eines** Aufrufs — nur für einen Exec-Effekt mit Pre- und
/// Post-Zeit ([`ToolCall::at`], [`ToolCall::ended_at`]). Fehlt eine (kein
/// eindeutiges Post-Event, Session von vor den Feldern), gibt es kein
/// Fenster: Über die Dauer wird nicht geraten (fail-closed). Ein Ende vor
/// dem Beginn (Uhr zurückgesprungen) öffnet ebenfalls keines. Den einen
/// Fall ohne Post-Zeit, der dennoch ein Fenster hat, kennt
/// [`exec_window_open`].
pub fn exec_window(call: &ToolCall) -> Option<(Timestamp, Timestamp)> {
    if call.effect.as_ref().map(|e| e.kind) != Some(EffectKind::Exec) {
        return None;
    }
    let started = time(call.at.as_deref())?;
    let ended = time(call.ended_at.as_deref())?;
    if ended < started {
        return None;
    }
    let from = started.checked_sub(EXEC_WINDOW_BEFORE).ok()?;
    let to = ended.checked_add(EXEC_WINDOW_AFTER).ok()?;
    Some((from, to))
}

/// Das Fenster des **letzten** Aufrufs einer Session, der beim Checkpoint
/// noch offen war: Pre-Zeit da, Post-Zeit nicht, kein Aufruf danach. Das
/// ist der Agent, der aus demselben Shell-Aufruf heraus committet (`cat >
/// … <<EOF && git add && git commit`): Der Commit versiegelt die Session,
/// bevor sein `PostToolUse` eintrifft — das landet im nächsten Bereich,
/// ohne Pre-Event, und erzeugt dort keinen Aufruf. Bis zum Seal lief aber
/// genau dieser Aufruf, also gehört ihm alles, was der Witness ab seinem
/// Beginn sah: `[at − EXEC_WINDOW_BEFORE, ∞)` — begrenzt durch die
/// Beobachtungen, die der Aufrufer ohnehin auf die Fenster der Sessions
/// beschränkt ([`observations_in_windows`]). Ein offener Aufruf, dem noch
/// ein Aufruf folgt (parallele Aufrufe), bekommt weiterhin keines.
pub fn exec_window_open(call: &ToolCall) -> Option<(Timestamp, Timestamp)> {
    if call.effect.as_ref().map(|e| e.kind) != Some(EffectKind::Exec) || call.ended_at.is_some() {
        return None;
    }
    let started = time(call.at.as_deref())?;
    let from = started.checked_sub(EXEC_WINDOW_BEFORE).ok()?;
    Some((from, Timestamp::MAX))
}

/// Die Fenster aller Shell-Aufrufe der `sessions`, in Session-, Turn- und
/// Aufruf-Reihenfolge — samt dem offenen Fenster eines letzten Aufrufs
/// ohne Post-Zeit ([`exec_window_open`]). Sessions ohne kanonische Id
/// tragen nichts bei.
pub fn exec_windows(sessions: &[&Session]) -> Vec<ExecWindow> {
    let mut out = Vec::new();
    for session in sessions {
        let Ok(id) = SessionId::of(*session) else {
            continue;
        };
        let last = session
            .turns
            .iter()
            .enumerate()
            .rev()
            .find_map(|(t, turn)| {
                (!turn.tool_calls.is_empty()).then(|| (t, turn.tool_calls.len() - 1))
            });
        for (turn_index, turn) in session.turns.iter().enumerate() {
            for (call_index, call) in turn.tool_calls.iter().enumerate() {
                let window = exec_window(call).or_else(|| {
                    (last == Some((turn_index, call_index)))
                        .then(|| exec_window_open(call))
                        .flatten()
                });
                if let Some((from, to)) = window {
                    out.push(ExecWindow {
                        source: LineSource {
                            session: id,
                            turn: turn_index,
                            call: call_index,
                        },
                        from,
                        to,
                    });
                }
            }
        }
    }
    out
}

/// Der Shell-Aufruf, in dessen Fenster eine Beobachtung um `at` fällt.
///
/// Genau ein Fenster: dieser Aufruf. Mehrere (parallel laufende Aufrufe,
/// ein Zug, der sie dicht hintereinander absetzt): `prefer` entscheidet —
/// typisch „der Aufruf erwähnt den Pfad" — aber nur, wenn es danach genau
/// einen gibt. Bleibt es mehrdeutig, wird keiner genannt (fail-closed): Die
/// Beobachtung ist dann weiterhin eine Beobachtung, nur ohne Aufruf.
pub fn exec_call_at(
    windows: &[ExecWindow],
    at: Timestamp,
    prefer: &dyn Fn(LineSource) -> bool,
) -> Option<LineSource> {
    let candidates: Vec<LineSource> = windows
        .iter()
        .filter(|w| w.from <= at && at <= w.to)
        .map(|w| w.source)
        .collect();
    match candidates.as_slice() {
        [] => None,
        [one] => Some(*one),
        several => {
            let preferred: Vec<LineSource> =
                several.iter().copied().filter(|s| prefer(*s)).collect();
            match preferred.as_slice() {
                [one] => Some(*one),
                _ => None,
            }
        }
    }
}

/// Die Beobachtungen aller vertrauenswürdigen `witness-fs/v1`-Epochen, die
/// sich mit `[from, to]` überschneiden, nach Zeit sortiert — einzeln auf das
/// Fenster gefiltert. Unlesbares, Manipuliertes und Unbestätigtes fällt weg
/// (fail-soft beim Lesen, fail-closed in der Wirkung).
///
/// `trusted(seal_id, seal_text)` wird gefragt, **bevor** ein Objekt geladen
/// wird — ein untergeschobenes Objekt kostet so nicht einmal das Lesen —,
/// und nur für Seals, deren Epoche nicht vor dem Fenster endete.
pub fn observations_in_window(
    store: &dyn ContextStore,
    from: Timestamp,
    to: Timestamp,
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> Vec<FsObservation> {
    observations_in_windows(
        store,
        &[Window {
            from,
            to,
            epochs: None,
        }],
        trusted,
    )
}

/// Ein Beobachtungsfenster: Zeitraum und — für die Fenster einer bezeugten
/// Session — genau die Epochen, deren Beobachtungen zählen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub from: Timestamp,
    pub to: Timestamp,
    /// Die `witness-fs/v1`-Seals, deren Beobachtungen in diesem Fenster
    /// zählen; `None`: jede vertrauenswürdige Epoche.
    pub epochs: Option<std::collections::BTreeSet<ContentHash>>,
}

/// Wie [`observations_in_window`], über die **Vereinigung** mehrerer
/// Fenster — eines je bezeugter Session. Die Lücke zwischen zwei Sessions
/// gehört zu keinem Fenster: Was der Witness dort sah (ein Mensch, ein
/// zurückgelassener Prozess), erklärt keine Zeile. Nennt ein Fenster seine
/// Epochen, zählen nur deren Beobachtungen.
pub fn observations_in_windows(
    store: &dyn ContextStore,
    windows: &[Window],
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> Vec<FsObservation> {
    observations_in_windows_checked(store, windows, trusted).observations
}

/// Die Beobachtungen der Fenster und ob sie vollständig gelesen wurden.
/// Bewusst ohne `Default`: „vollständig" muss jeder Aufrufer aussprechen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowObservations {
    /// Was lesbar war — wie [`observations_in_windows`].
    pub observations: Vec<FsObservation>,
    /// `false`: Ein vertrauenswürdiger `witness-fs/v1`-Seal, dessen Epoche
    /// in ein Fenster fallen kann, nennt ein Beobachtungs-Objekt, das fehlt
    /// oder nicht lesbar ist, oder zählt Lücken (verlorene Ereignisse) —
    /// oder die Seals selbst waren nicht auflistbar.
    /// Unter `refs/minds/` kann auch der Agent schreiben: Wer ein Objekt
    /// entfernt, entfernt Beobachtungen. Für eine Aussage über **alles**,
    /// was beobachtet wurde (Scope-Befunde, EA-17), ist das Material dann
    /// unvollständig.
    pub complete: bool,
    /// Wo der Beobachter in den Fenstern blind war
    /// ([`minds_core::observation::ObservationGap`] der genannten Epochen),
    /// nach Zeit sortiert. Jede macht das Material unvollständig — und
    /// erklärt einem Claim ohne Beobachtung, warum
    /// ([`gap_near`]).
    pub gaps: Vec<FsGap>,
}

/// Eine Lücke des Beobachters, gelesen: wann, warum, wie viele Meldungen
/// (wo zählbar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsGap {
    pub at: Timestamp,
    pub reason: GapReason,
    pub dropped: Option<u64>,
}

impl FsGap {
    /// Der Text für ein Urteil: `overflow: 12 events dropped`, sonst nur
    /// das Grund-Wort.
    pub fn describe(&self) -> String {
        match self.dropped {
            Some(n) => format!("{}: {n} events dropped", self.reason.word()),
            None => self.reason.word().to_owned(),
        }
    }
}

/// Die Lücke, in die die Beobachtung eines Claims um `claim_at` gefallen
/// sein kann: eine im Fenster `[claim − 2 s, claim + 30 s]` — dasselbe
/// Fenster wie bei [`corroborate`]. Die erste nach Zeit.
pub fn gap_near(gaps: &[FsGap], claim_at: Timestamp) -> Option<&FsGap> {
    let (Ok(from), Ok(to)) = (
        claim_at.checked_sub(CORROBORATION_BEFORE),
        claim_at.checked_add(CORROBORATION_AFTER),
    ) else {
        return None;
    };
    gaps.iter().find(|gap| from <= gap.at && gap.at <= to)
}

/// Fenster und Beobachtungen **einer** Session, wie `minds verify` sie
/// liest: [`witness_windows`], dann [`observations_in_windows_checked`].
///
/// Ohne Fenster gibt es keine Beobachtungen. Hat der Witness die Session
/// aber bezeugt ([`witnessed_sessions`]), fehlt dann ein Glied seiner
/// Beobachtungskette — etwa das (vom Agenten gelöschte) Objekt der ersten
/// Epoche, ein Neustart mitten in der Session, ein Block-Seal: Das ist
/// „unvollständig", nicht „nichts beobachtet" (fail-closed).
pub fn session_observations(
    store: &dyn ContextStore,
    id: SessionId,
    session: &Session,
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> (Vec<Window>, WindowObservations) {
    let windows = witness_windows(store, &[(id, session)], trusted);
    if windows.is_empty() {
        // Ist der Store nicht auflistbar, ist auch „nicht bezeugt" nicht
        // belegt.
        let witnessed =
            store.list_seals().is_err() || !witnessed_sessions(store, &[id], trusted).is_empty();
        return (
            windows,
            WindowObservations {
                observations: Vec::new(),
                complete: !witnessed,
                gaps: Vec::new(),
            },
        );
    }
    let observations = observations_in_windows_checked(store, &windows, trusted);
    (windows, observations)
}

/// Wie [`observations_in_windows`], meldet aber, ob jedes in Frage
/// kommende Objekt lesbar war ([`WindowObservations::complete`]). In Frage
/// kommt ein Objekt, dessen Seal vertrauenswürdig ist, dessen Epoche nicht
/// vor dem frühesten Fenster endete und — nennen alle Fenster ihre Epochen —
/// zu einer dieser Epochen gehört. Ob seine Beobachtungen ins Fenster
/// fielen, ist ohne das Objekt nicht entscheidbar: fail-closed.
pub fn observations_in_windows_checked(
    store: &dyn ContextStore,
    windows: &[Window],
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> WindowObservations {
    let named = |seal_id: &ContentHash| {
        windows.iter().any(|w| {
            w.epochs
                .as_ref()
                .is_none_or(|epochs| epochs.contains(seal_id))
        })
    };
    let inside = |seal_id: &ContentHash, at: Timestamp| {
        windows.iter().any(|w| {
            w.from <= at
                && at <= w.to
                && w.epochs
                    .as_ref()
                    .is_none_or(|epochs| epochs.contains(seal_id))
        })
    };
    let Some(from) = windows.iter().map(|w| w.from).min() else {
        return WindowObservations {
            observations: Vec::new(),
            complete: true,
            gaps: Vec::new(),
        };
    };
    let Ok(seals) = store.list_seals() else {
        return WindowObservations {
            observations: Vec::new(),
            complete: false,
            gaps: Vec::new(),
        };
    };
    let mut complete = true;
    let mut out: Vec<(Timestamp, FsObservation)> = Vec::new();
    let mut gaps: Vec<FsGap> = Vec::new();
    let mut seen_objects = std::collections::BTreeSet::new();
    for seal_id in seals {
        // Ein aufgelisteter, aber nicht lesbarer Seal kann eine Epoche des
        // Fensters sein: unvollständig (fail-closed).
        let Ok(Some(text)) = store.seal_text(&seal_id) else {
            complete = false;
            continue;
        };
        let Ok(seal) = Seal::parse(&text) else {
            continue;
        };
        let SealOutcome::ObservationsStored { observations } = &seal.outcome else {
            continue;
        };
        if seal.scope != SCOPE_WITNESS_FS_V1
            || time(Some(&seal.last_event_at)).is_some_and(|last| last < from)
            || !trusted(&seal_id, &text)
        {
            continue;
        }
        // Eine Epoche mit Lücken: Der Witness hat Ereignisse verloren.
        if seal.gaps > 0 {
            complete &= !named(&seal_id);
        }
        // Ein bezeugter Seal nennt ein Objekt, das hier fehlt oder nicht
        // lesbar ist.
        let Ok(object_id) = observations.parse::<ContentHash>() else {
            complete &= !named(&seal_id);
            continue;
        };
        let Ok(Some(object)) = store.get_observations(&object_id) else {
            complete &= !named(&seal_id);
            continue;
        };
        // Aus den Beobachtungen selbst, nicht aus `first_at`/`last_at` (die
        // folgen der `seq`-Ordnung — springt die Uhr zurück, sind sie nicht
        // Minimum und Maximum).
        let overlaps = object
            .observations
            .iter()
            .filter_map(|o| time(Some(&o.at)))
            .any(|at| inside(&seal_id, at));
        // Der Beobachter war in dieser Epoche blind: unvollständig — und
        // die Lücken im Fenster werden genannt.
        if !object.gaps.is_empty() && named(&seal_id) {
            complete = false;
        }
        for gap in &object.gaps {
            if let Some(at) = time(Some(&gap.at))
                && inside(&seal_id, at)
            {
                gaps.push(FsGap {
                    at,
                    reason: gap.reason,
                    dropped: gap.dropped,
                });
            }
        }
        if !overlaps || !seen_objects.insert(object_id) {
            continue;
        }
        for observation in object.observations {
            let Some(at) = time(Some(&observation.at)) else {
                continue;
            };
            if !inside(&seal_id, at) {
                continue;
            }
            let (hash, opaque) = match (observation.content, observation.reason) {
                (Some(hash), _) => (Some(hash), false),
                (None, Some(ObservationReason::Deleted)) => (None, false),
                (None, _) => (None, true),
            };
            out.push((
                at,
                FsObservation {
                    path: observation.path,
                    observed: ObservedAt {
                        hash,
                        seq: observation.seq,
                        at: Some(observation.at),
                    },
                    content: None,
                    opaque,
                },
            ));
        }
    }
    out.sort_by(|a, b| (a.0, &a.1.path, a.1.observed.seq).cmp(&(b.0, &b.1.path, b.1.observed.seq)));
    gaps.sort_by_key(|gap| gap.at);
    gaps.dedup();
    WindowObservations {
        observations: out.into_iter().map(|(_, o)| o).collect(),
        complete,
        gaps,
    }
}

/// Das Fenster der verknüpften Sessions: vom frühesten bis zum spätesten
/// bekannten Zeitpunkt (Start, Züge, Ende), erweitert um [`WINDOW_SLACK`].
/// `None`, wenn keine Session einen lesbaren Zeitpunkt trägt.
pub fn session_window(sessions: &[&Session]) -> Option<(Timestamp, Timestamp)> {
    let mut times = Vec::new();
    for session in sessions {
        if let Some(lineage) = &session.lineage {
            times.extend(time(lineage.started_at.as_deref()));
            times.extend(time(lineage.ended_at.as_deref()));
        }
        times.extend(session.turns.iter().filter_map(|t| time(t.at.as_deref())));
    }
    let from = times.iter().min()?.checked_sub(WINDOW_SLACK).ok()?;
    let to = times.iter().max()?.checked_add(WINDOW_SLACK).ok()?;
    Some((from, to))
}

/// Höchstzahl der Epochen, die eine Kette zurückverfolgt wird. Seals sind
/// inhaltsadressiert, ein Zyklus ist nicht konstruierbar; die Grenze
/// begrenzt nur die Laufzeit, und wer sie sprengt, bekommt kein Fenster.
const MAX_CHAIN: usize = 100_000;

/// Die Beobachtungsfenster der verknüpften `sessions` — eines je vom Witness
/// bezeugter Session (siehe [`witnessed_sessions`]); unbezeugte öffnen keins.
/// Die Ids sind die des Stores, nicht neu berechnet: Eine tolerant gelesene
/// Session aus einem neueren Binary hätte sonst eine andere.
///
/// - **Ende:** die Epoche des Checkpoints, der die Session versiegelte — die
///   früheste **vertrauenswürdige** `witness-fs/v1`-Epoche, deren
///   `last_event_at` nicht vor dem bezeugten Bereich liegt. Der Witness
///   versiegelt sie vor den Sessions und schließt sie mit einem monoton
///   gestempelten `fs.checkpoint`-Event ab: Was nach dem Commit geschah,
///   liegt in der nächsten. Untergeschobene (unbestätigte) Seals zählen
///   nicht — weder als Ende noch als Hindernis.
/// - **Epochen:** Von dieser Epoche aus führt die signierte `previous`-Kette
///   zurück, bis sie vollständig ist: an einer Epoche, die **vor** dem
///   Fensterbeginn endet, oder an ihrem Anfang (`previous = None`, der erste
///   Checkpoint nach einem Witness-Start), wenn dessen Objekt belegt, dass
///   die Epoche **nicht nach** dem Fensterbeginn begann (`started_at`,
///   EA-08a — über die Objekt-Id im Seal mitsigniert). Nur die Beobachtungen
///   der Epochen dazwischen (samt Anfang) zählen. Begann der Anfang später
///   (Neustart während der Session) oder trägt sein Objekt keinen Beginn
///   (Schema 1), ist nicht belegt, dass die Kette vor dem Commit begann —
///   kein Fenster; die Session steht wie ohne Witness da.
/// - **Beginn:** der früheste Zeitpunkt der Session, minus [`WINDOW_SLACK`]
///   — vom Witness gestempelt.
///
/// Fehlt ein Glied, ist eines nicht vertrauenswürdig oder ein Block-Seal
/// (Beobachtungen nicht abgelegt), gibt es für die Session **kein** Fenster:
/// Die jüngste vorhandene Beobachtung wäre sonst nicht die jüngste, und ein
/// früher passender Stand könnte einen späteren Widerspruch verdecken. Ohne
/// Fenster gilt, was ohne Witness gilt. Wer Seals löscht oder unterschiebt,
/// kann eine Session so auf den Stand ohne Witness zurückwerfen — nie eine
/// Zeile zu `explained` machen.
pub fn witness_windows(
    store: &dyn ContextStore,
    sessions: &[(SessionId, &Session)],
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> Vec<Window> {
    struct Epoch {
        last: Timestamp,
        text: String,
        previous: Option<ContentHash>,
        /// Das Observation-Objekt; `None` bei einem Block-Seal.
        object: Option<String>,
    }
    let ids: Vec<SessionId> = sessions.iter().map(|(id, _)| *id).collect();
    let witnessed = witnessed_sessions(store, &ids, trusted);
    if witnessed.is_empty() {
        return Vec::new();
    }
    let mut epochs: std::collections::HashMap<ContentHash, Epoch> =
        std::collections::HashMap::new();
    for seal_id in store.list_seals().unwrap_or_default() {
        let Ok(Some(text)) = store.seal_text(&seal_id) else {
            continue;
        };
        let Ok(seal) = Seal::parse(&text) else {
            continue;
        };
        let object = match seal.outcome {
            SealOutcome::ObservationsStored { observations } => Some(observations),
            SealOutcome::Rejected => None,
            SealOutcome::Stored { .. } => continue,
        };
        if seal.scope != SCOPE_WITNESS_FS_V1 {
            continue;
        }
        if let Some(last) = time(Some(&seal.last_event_at)) {
            epochs.insert(
                seal_id,
                Epoch {
                    last,
                    text,
                    previous: seal.previous,
                    object,
                },
            );
        }
    }
    let mut ends: Vec<(Timestamp, ContentHash)> = epochs
        .iter()
        .map(|(id, epoch)| (epoch.last, id.clone()))
        .collect();
    ends.sort();
    let verdicts: std::cell::RefCell<std::collections::HashMap<ContentHash, bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    let check = |seal_id: &ContentHash, text: &str| {
        if let Some(verdict) = verdicts.borrow().get(seal_id) {
            return *verdict;
        }
        let verdict = trusted(seal_id, text);
        verdicts.borrow_mut().insert(seal_id.clone(), verdict);
        verdict
    };
    let mut windows = Vec::new();
    for (id, session) in sessions {
        let Some(until) = witnessed
            .iter()
            .find(|(witnessed, _)| witnessed == id)
            .map(|(_, until)| *until)
        else {
            continue;
        };
        let Some((from, _)) = session_window(&[*session]) else {
            continue;
        };
        let Some((end, end_id)) =
            ends.iter()
                .filter(|(last, _)| *last >= until)
                .find(|(_, seal_id)| {
                    epochs
                        .get(seal_id)
                        .is_some_and(|epoch| check(seal_id, &epoch.text))
                })
        else {
            continue;
        };
        // Vollständig ist die Kette, wenn sie eine Epoche erreicht, die
        // **vor** dem Fensterbeginn endet — oder ihren Anfang, sofern der
        // belegt nicht nach dem Fensterbeginn begann. Ohne diesen Beleg
        // könnte ein Anfang nach einem Neustart und einer gelöschten Epoche
        // des alten Laufs ganz nach dem Commit liegen.
        let mut chain = std::collections::BTreeSet::new();
        let mut cursor = Some(end_id.clone());
        let mut complete = false;
        while let Some(seal_id) = cursor {
            let Some(epoch) = epochs.get(&seal_id) else {
                complete = false;
                break;
            };
            if !check(&seal_id, &epoch.text) || chain.len() >= MAX_CHAIN {
                complete = false;
                break;
            }
            if epoch.last < from {
                complete = true;
                break;
            }
            let Some(object) = &epoch.object else {
                // Block-Seal: Beobachtungen fehlen.
                complete = false;
                break;
            };
            if epoch.previous.is_none() {
                complete = began_by(store, object, from);
                chain.insert(seal_id);
                break;
            }
            cursor = epoch.previous.clone();
            // Schon gesehen: ein Zyklus (per Inhaltsadresse unmöglich — die
            // Prüfung verlässt sich trotzdem nicht darauf).
            if !chain.insert(seal_id) {
                complete = false;
                break;
            }
        }
        if complete && from <= *end {
            windows.push(Window {
                from,
                to: *end,
                epochs: Some(chain),
            });
        }
    }
    windows
}

/// Ob die Epoche mit dem Observation-Objekt `object` belegt spätestens um
/// `from` begann: Das Objekt liegt vor, besteht die Hash-Prüfung des Stores,
/// hat genau Schema 2 und trägt einen lesbaren `started_at <= from`.
/// Alles andere verankert nicht — Schema 1 (kein Beginn) ebenso wie ein
/// künftiges Schema, dessen Bedeutung von `started_at` dieses Binary nicht
/// kennt (fail-closed; wer das Schema anhebt, nimmt es hier auf).
fn began_by(store: &dyn ContextStore, object: &str, from: Timestamp) -> bool {
    let Ok(id) = object.parse::<ContentHash>() else {
        return false;
    };
    let Ok(Some(object)) = store.get_observations(&id) else {
        return false;
    };
    object.schema == 2 && object.started_at().is_some_and(|start| start <= from)
}

/// Was der Witness in den Epochen der Fenster über sich festhielt — je
/// genannter Epoche (doppelte einmal) ihre Seal-Id und die Fakten, `None`,
/// wo das Objekt fehlt, nicht lesbar ist oder keine Fakten trägt (ältere
/// Objekte).
///
/// Die Fenster stammen aus [`witness_windows`]: Ihre Epochen sind schon
/// vertrauenswürdige `witness-fs/v1`-Seals. Ein Fenster ohne benannte
/// Epochen trägt nichts bei — der Aufrufer behandelt eine leere Liste als
/// „Profil unbekannt" (fail-closed).
pub fn witness_facts(
    store: &dyn ContextStore,
    windows: &[Window],
) -> Vec<(ContentHash, Option<minds_core::observation::WitnessFacts>)> {
    let epochs: std::collections::BTreeSet<&ContentHash> = windows
        .iter()
        .flat_map(|window| window.epochs.iter().flatten())
        .collect();
    epochs
        .into_iter()
        .map(|epoch| {
            let facts = (|| {
                let text = store.seal_text(epoch).ok().flatten()?;
                let seal = Seal::parse(&text).ok()?;
                let SealOutcome::ObservationsStored { observations } = &seal.outcome else {
                    return None;
                };
                let id = observations.parse::<ContentHash>().ok()?;
                store.get_observations(&id).ok().flatten()?.witness
            })();
            (epoch.clone(), facts)
        })
        .collect()
}

/// Die Witness-Starts für die Assurance ([`crate::assurance::WitnessStart`])
/// aus [`witness_facts`]: das Profil als Wort gelesen (unbekannt ⇒ `None`).
/// Eine Epoche ohne Fakten wird ein Start ohne Profil — nie übergangen.
///
/// **Getrennt** ist ein Lauf nur, wenn sein Befund die Trennung zeigt
/// ([`minds_core::observation::Isolation::proven`]), das Profil bekannt ist
/// **und** `separated(epoch)` gilt: Der Seal der Epoche ist unter einem
/// Principal gültig, den ein Mensch als getrennten Witness eingetragen hat
/// ([`minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL`]). Die Fakten
/// schreibt der Witness selbst — ohne diese Bürgschaft könnte der Schlüssel
/// eines Witness unter der Kennung des Agenten sie behaupten.
pub fn witness_starts(
    facts: &[(ContentHash, Option<minds_core::observation::WitnessFacts>)],
    separated: &dyn Fn(&ContentHash) -> bool,
) -> Vec<crate::assurance::WitnessStart> {
    facts
        .iter()
        .map(|(epoch, facts)| {
            let profile = facts
                .as_ref()
                .and_then(|f| minds_core::evidence::WitnessProfile::parse(&f.profile));
            crate::assurance::WitnessStart {
                profile,
                isolated: profile.is_some()
                    && facts.as_ref().is_some_and(|f| f.isolation.proven())
                    && separated(epoch),
                key: None,
            }
        })
        .collect()
}

/// Die vom Witness bezeugten unter den `sessions`, je mit dem Ende ihres
/// versiegelten Bereichs: Sessions, die ein vertrauenswürdiger
/// `witness/v1`-Seal mit `outcome=stored` **namentlich** nennt, und der
/// späteste `last_event_at` der Seals, die **diese** Session nennen.
///
/// Gesucht wird über alle Seals, nicht über die Rückverweise einer Session:
/// Die liegen veränderbar im geteilten Repo, und ein dort eingetragener
/// fremder Seal verschöbe das Ende.
///
/// Warum nur bezeugte Sessions: Das Beobachtungsfenster stützt sich auf
/// Zeitpunkte der Sessions. Bei einer Session, die nur lokal (oder gar vom
/// Agenten selbst) erfasst wurde, kann der Agent diese Zeitpunkte setzen —
/// ein `started_at` vor Jahren oder ein Zug in der Zukunft öffnete das
/// Fenster über die ganze Historie des Witness, und echte Beobachtungen
/// menschlicher Änderungen „erklärten" Zeilen. Bei einer bezeugten Session
/// stempelt der Witness die Zeiten selbst.
pub fn witnessed_sessions(
    store: &dyn ContextStore,
    sessions: &[SessionId],
    trusted: &dyn Fn(&ContentHash, &str) -> bool,
) -> Vec<(SessionId, Timestamp)> {
    let mut witnessed: std::collections::BTreeMap<SessionId, Timestamp> =
        std::collections::BTreeMap::new();
    let Ok(seals) = store.list_seals() else {
        return Vec::new();
    };
    for seal_id in seals {
        let Ok(Some(text)) = store.seal_text(&seal_id) else {
            continue;
        };
        let Ok(seal) = Seal::parse(&text) else {
            continue;
        };
        let SealOutcome::Stored { session } = &seal.outcome else {
            continue;
        };
        let Ok(id) = session.parse::<SessionId>() else {
            continue;
        };
        if seal.scope != SCOPE_WITNESS_V1 || !sessions.contains(&id) || !trusted(&seal_id, &text) {
            continue;
        }
        let Some(last) = time(Some(&seal.last_event_at)) else {
            continue;
        };
        let until = witnessed.entry(id).or_insert(last);
        *until = (*until).max(last);
    }
    witnessed.into_iter().collect()
}

fn time(at: Option<&str>) -> Option<Timestamp> {
    at.and_then(|at| at.parse().ok())
}

/// Ob ein unabhängiger Beobachter einen Schreib-Claim bestätigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corroboration {
    /// Der Witness sah genau diesen Inhalt an genau diesem Pfad im Fenster
    /// `[claim − 2 s, claim + 30 s]`.
    Corroborated,
    /// Kein passender Beleg — ein Claim ohne Zeugen (oder ein erfundener).
    Uncorroborated,
    /// Nicht prüfbar: kein Schreibzeit-Hash, oder der Pfad liegt außerhalb
    /// des Repositorys (dort beobachtet der Witness nicht).
    NotApplicable,
}

impl Corroboration {
    /// Das Anzeige-Wort (`00-conventions.md`).
    pub const fn word(self) -> &'static str {
        match self {
            Self::Corroborated => "corroborated",
            Self::Uncorroborated => "uncorroborated",
            Self::NotApplicable => "not applicable",
        }
    }
}

/// Die Korroborations-Regel für **einen** Claim: Hash `written` an Pfad
/// `path` (repo-relativ), behauptet um `claim_at`. Ohne Zeitpunkt kein
/// Fenster — und damit keine Bestätigung.
pub fn corroborate(
    path: &str,
    written: &ContentHash,
    claim_at: Option<Timestamp>,
    observations: &[FsObservation],
) -> Corroboration {
    let Some(claim_at) = claim_at else {
        return Corroboration::Uncorroborated;
    };
    let (Ok(from), Ok(to)) = (
        claim_at.checked_sub(CORROBORATION_BEFORE),
        claim_at.checked_add(CORROBORATION_AFTER),
    ) else {
        return Corroboration::Uncorroborated;
    };
    let seen = observations.iter().any(|o| {
        !o.opaque
            && o.path == path
            && o.observed.hash.as_ref() == Some(written)
            && time(o.observed.at.as_deref()).is_some_and(|at| from <= at && at <= to)
    });
    if seen {
        Corroboration::Corroborated
    } else {
        Corroboration::Uncorroborated
    }
}

/// Ein Schreib-Claim samt Korroboration — für EA-11/EA-12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCorroboration {
    pub session: SessionId,
    pub turn: usize,
    pub call: usize,
    /// Repo-relativ, sofern abbildbar — unentschärft (Identität, kein
    /// Anzeigetext).
    pub path: Option<String>,
    pub corroboration: Corroboration,
}

/// Die Korroboration jedes Schreib-Claims der `sessions` gegen
/// `observations`. `roots` wie bei [`claim_path`]; Kandidaten des
/// Erfassungszeit-Fallbacks bestätigt nur ein beobachteter Pfad. Rein,
/// deterministisch.
///
/// Grenzen, die das Ergebnis mitprägen:
/// - Der Zeitpunkt eines Claims ist der seiner Meldung
///   ([`minds_core::Effect::written_at`], das `PostToolUse`-Event). Fehlt
///   er (Sessions von vor diesem Feld), gilt der des **Zugs** (`turn.at`):
///   Liegen die Tool-Calls eines Zugs dann über mehr als 30 s verteilt,
///   fällt ein später Schreibzugriff aus dem Fenster und bleibt unbestätigt.
/// - Der Witness hält Inhalts**änderungen** fest: Schreibt jemand innerhalb
///   einer Epoche denselben Inhalt erneut, entsteht keine zweite
///   Beobachtung; ein Claim dafür findet nur die erste.
pub fn corroborations(
    sessions: &[&Session],
    roots: &[&std::path::Path],
    observations: &[FsObservation],
) -> Vec<ClaimCorroboration> {
    let observed: std::collections::BTreeSet<&str> =
        observations.iter().map(|o| o.path.as_str()).collect();
    let known = |path: &str| observed.contains(path);
    let mut out = Vec::new();
    for session in sessions {
        let Ok(id) = SessionId::of(*session) else {
            continue;
        };
        let cwd = session.lineage.as_ref().and_then(|l| l.cwd.as_deref());
        for (turn_index, turn) in session.turns.iter().enumerate() {
            let at = time(turn.at.as_deref());
            for (call_index, call) in turn.tool_calls.iter().enumerate() {
                let Some(effect) = &call.effect else { continue };
                if effect.kind != EffectKind::Write {
                    continue;
                }
                let path = effect
                    .path
                    .as_deref()
                    .and_then(|p| claim_path(p, cwd, roots, &known, false));
                let claimed_at = time(effect.written_at.as_deref()).or(at);
                let corroboration = match (&path, &effect.written) {
                    (Some(path), Some(written)) => {
                        corroborate(path, written, claimed_at, observations)
                    }
                    _ => Corroboration::NotApplicable,
                };
                out.push(ClaimCorroboration {
                    session: id,
                    turn: turn_index,
                    call: call_index,
                    path,
                    corroboration,
                });
            }
        }
    }
    out.sort_by_key(|c| (c.session, c.turn, c.call));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fakten werden Starts: Profil als Wort (unbekannt ⇒ `None`), Trennung
    /// nur mit Befund, bekanntem Profil und Bürgschaft des Principals; eine
    /// Epoche ohne Fakten bleibt ein Start ohne Profil.
    #[test]
    fn witness_facts_become_starts() {
        use minds_core::evidence::WitnessProfile;
        use minds_core::observation::{Isolation, WitnessFacts};
        let facts = |profile: &str, witness_uid: u32| WitnessFacts {
            profile: profile.into(),
            isolation: Isolation {
                witness_uid,
                agent_uids: vec![501],
                home_private: true,
                binary_protected: true,
            },
            spec: None,
        };
        let epoch = |n: u8| ContentHash::from_bytes([n; 32]);
        let facts = [
            (epoch(1), Some(facts("user", 498))),
            (epoch(2), Some(facts("local", 501))),
            (epoch(3), Some(facts("sandbox", 498))),
            (epoch(4), None),
            (epoch(5), Some(facts("user", 498))),
        ];
        // Epoche 5 gilt nur unter einem gewöhnlichen Witness-Principal.
        let separated = |seal: &ContentHash| *seal != epoch(5);
        let pairs: Vec<(Option<WitnessProfile>, bool)> = witness_starts(&facts, &separated)
            .iter()
            .map(|s| (s.profile, s.isolated))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some(WitnessProfile::User), true),
                (Some(WitnessProfile::Local), false),
                (None, false),
                (None, false),
                (Some(WitnessProfile::User), false),
            ]
        );
    }

    fn h(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn obs(path: &str, hash: Option<ContentHash>, at: &str, opaque: bool) -> FsObservation {
        FsObservation {
            path: path.into(),
            observed: ObservedAt {
                hash,
                seq: 0,
                at: Some(at.into()),
            },
            content: None,
            opaque,
        }
    }

    #[test]
    fn corroboration_window_rules() {
        let claim: Timestamp = "2026-10-05T10:00:10Z".parse().unwrap();
        let at = |s: &str| obs("src/a.rs", Some(h(1)), s, false);
        // Innerhalb [−2 s, +30 s], beide Grenzen eingeschlossen.
        for inside in [
            "2026-10-05T10:00:08Z",
            "2026-10-05T10:00:10Z",
            "2026-10-05T10:00:40Z",
        ] {
            assert_eq!(
                corroborate("src/a.rs", &h(1), Some(claim), &[at(inside)]),
                Corroboration::Corroborated,
                "{inside}"
            );
        }
        for outside in ["2026-10-05T10:00:07.999Z", "2026-10-05T10:00:40.001Z"] {
            assert_eq!(
                corroborate("src/a.rs", &h(1), Some(claim), &[at(outside)]),
                Corroboration::Uncorroborated,
                "{outside}"
            );
        }
        let ok = "2026-10-05T10:00:11Z";
        // Anderer Hash, anderer Pfad, opak, ohne Claim-Zeit: keine Bestätigung.
        for (path, hash, opaque, when) in [
            ("src/a.rs", Some(h(2)), false, Some(claim)),
            ("src/b.rs", Some(h(1)), false, Some(claim)),
            ("src/a.rs", None, true, Some(claim)),
            ("src/a.rs", Some(h(1)), false, None),
        ] {
            assert_eq!(
                corroborate("src/a.rs", &h(1), when, &[obs(path, hash, ok, opaque)]),
                Corroboration::Uncorroborated
            );
        }
        assert_eq!(Corroboration::Uncorroborated.word(), "uncorroborated");
    }

    #[test]
    fn corroborations_cover_every_write_claim() {
        use minds_core::{Agent, Effect, Intent, Model, Role, ToolCall, Turn};
        let call_at = |path: &str, written: Option<ContentHash>, at: Option<&str>| ToolCall {
            at: None,
            ended_at: None,
            outcome: None,
            name: "Write".into(),
            arguments: String::new(),
            capture: None,
            effect: Some(Effect {
                kind: EffectKind::Write,
                path: Some(path.into()),
                content: None,
                written,
                written_unavailable: None,
                written_at: at.map(str::to_owned),
            }),
        };
        let call = |path: &str, written: Option<ContentHash>| call_at(path, written, None);
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        for (at, calls) in [
            (
                Some("2026-10-05T10:00:10Z"),
                vec![
                    // Gesehen: bestätigt.
                    call("/repo/src/a.rs", Some(h(1))),
                    // Kein Schreibzeit-Hash (Secret, unscanbar): nicht prüfbar.
                    call("/repo/.env", None),
                    // Außerhalb des Repos: dort beobachtet niemand.
                    call("/elsewhere/x.rs", Some(h(1))),
                    // Behauptet, aber nie gesehen.
                    call("/repo/src/b.rs", Some(h(2))),
                ],
            ),
            // Ohne Zeitpunkt kein Fenster.
            (None, vec![call("/repo/src/a.rs", Some(h(1)))]),
            // Ein Zug, der Minuten dauert: Der Claim trägt die Zeit seiner
            // Meldung, und die zählt — nicht der Beginn des Zugs. Ohne sie
            // (ältere Sessions) bleibt es beim Zug.
            (
                Some("2026-10-05T10:00:10Z"),
                vec![
                    call_at("/repo/src/c.rs", Some(h(3)), Some("2026-10-05T10:04:00Z")),
                    call("/repo/src/d.rs", Some(h(4))),
                    // Die Meldung liegt vor der Beobachtung — erfunden.
                    call_at("/repo/src/c.rs", Some(h(3)), Some("2026-10-05T10:03:00Z")),
                ],
            ),
        ] {
            session.turns.push(Turn {
                role: Role::Assistant,
                text: String::new(),
                tool_calls: calls,
                parent: None,
                at: at.map(str::to_owned),
            });
        }
        let observations = [
            obs("src/a.rs", Some(h(1)), "2026-10-05T10:00:12Z", false),
            obs("src/c.rs", Some(h(3)), "2026-10-05T10:04:01Z", false),
            obs("src/d.rs", Some(h(4)), "2026-10-05T10:04:01Z", false),
        ];
        let root = std::path::Path::new("/repo");
        let got: Vec<(usize, usize, Option<String>, Corroboration)> =
            corroborations(&[&session], &[root], &observations)
                .into_iter()
                .map(|c| (c.turn, c.call, c.path, c.corroboration))
                .collect();
        assert_eq!(
            got,
            [
                (0, 0, Some("src/a.rs".into()), Corroboration::Corroborated),
                (0, 1, Some(".env".into()), Corroboration::NotApplicable),
                (0, 2, None, Corroboration::NotApplicable),
                (0, 3, Some("src/b.rs".into()), Corroboration::Uncorroborated),
                (1, 0, Some("src/a.rs".into()), Corroboration::Uncorroborated),
                (2, 0, Some("src/c.rs".into()), Corroboration::Corroborated),
                (2, 1, Some("src/d.rs".into()), Corroboration::Uncorroborated),
                (2, 2, Some("src/c.rs".into()), Corroboration::Uncorroborated),
            ]
        );
    }

    /// Ein Shell-Aufruf öffnet sein Fenster nur mit Pre- und Post-Zeit, nur
    /// als Exec, nie mit einem Ende vor dem Beginn; die Ränder sind
    /// `[at − 2 s, ended_at + 5 s]`.
    #[test]
    fn exec_windows_need_both_times_and_an_exec_effect() {
        use minds_core::Effect;
        let call = |kind: EffectKind, at: Option<&str>, ended: Option<&str>| ToolCall {
            at: at.map(str::to_owned),
            ended_at: ended.map(str::to_owned),
            outcome: None,
            name: "Bash".into(),
            arguments: String::new(),
            capture: None,
            effect: Some(Effect {
                kind,
                path: None,
                content: None,
                written: None,
                written_unavailable: None,
                written_at: None,
            }),
        };
        let (pre, post) = ("2026-10-05T10:00:10Z", "2026-10-05T10:00:12Z");
        let (from, to) = exec_window(&call(EffectKind::Exec, Some(pre), Some(post))).unwrap();
        assert_eq!(from.to_string(), "2026-10-05T10:00:08Z");
        assert_eq!(to.to_string(), "2026-10-05T10:00:17Z");
        for (kind, at, ended) in [
            (EffectKind::Exec, None, Some(post)),
            (EffectKind::Exec, Some(pre), None),
            (EffectKind::Exec, Some(post), Some(pre)),
            (EffectKind::Write, Some(pre), Some(post)),
            (EffectKind::Other, Some(pre), Some(post)),
        ] {
            assert_eq!(
                exec_window(&call(kind, at, ended)),
                None,
                "{kind:?} {at:?} {ended:?}"
            );
        }
        let mut plain = call(EffectKind::Exec, Some(pre), Some(post));
        plain.effect = None;
        assert_eq!(exec_window(&plain), None);
    }

    /// Der letzte Aufruf einer Session ohne Post-Zeit war beim Seal offen:
    /// Sein Fenster reicht vom Pre-Event bis ans Ende des Materials. Ein
    /// offener Aufruf mit Nachfolger und jeder Nicht-Exec bekommt keines.
    #[test]
    fn an_open_last_call_gets_a_window_to_the_end_of_the_material() {
        use minds_core::{Agent, Effect, Intent, Model, Role, Turn};
        let call = |kind: EffectKind, at: &str, ended: Option<&str>| ToolCall {
            at: Some(at.into()),
            ended_at: ended.map(str::to_owned),
            outcome: None,
            name: "Bash".into(),
            arguments: String::new(),
            capture: None,
            effect: Some(Effect {
                kind,
                path: None,
                content: None,
                written: None,
                written_unavailable: None,
                written_at: None,
            }),
        };
        let session = |calls: Vec<ToolCall>| {
            let mut s = Session::new(
                Agent {
                    name: "claude-code".into(),
                    version: "1".into(),
                },
                Model {
                    provider: "anthropic".into(),
                    id: "m".into(),
                },
                Intent::default(),
            );
            s.turns.push(Turn {
                role: Role::Assistant,
                text: String::new(),
                tool_calls: calls,
                parent: None,
                at: Some("2026-10-05T10:00:00Z".into()),
            });
            s
        };
        let open = call(EffectKind::Exec, "2026-10-05T10:00:10Z", None);
        assert_eq!(
            exec_window_open(&open).map(|(from, to)| (from.to_string(), to)),
            Some(("2026-10-05T10:00:08Z".to_string(), Timestamp::MAX))
        );
        assert_eq!(
            exec_window_open(&call(
                EffectKind::Exec,
                "2026-10-05T10:00:10Z",
                Some("2026-10-05T10:00:11Z")
            )),
            None,
            "with a post time the closed window applies"
        );
        assert_eq!(
            exec_window_open(&call(EffectKind::Write, "2026-10-05T10:00:10Z", None)),
            None
        );
        // Als letzter Aufruf: offen bis ∞. Mit einem Aufruf danach: nichts.
        let alone = session(vec![open.clone()]);
        let windows = exec_windows(&[&alone]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].to, Timestamp::MAX);
        assert_eq!(windows[0].source.call, 0);
        let followed = session(vec![
            open,
            call(
                EffectKind::Exec,
                "2026-10-05T10:00:20Z",
                Some("2026-10-05T10:00:21Z"),
            ),
        ]);
        let windows = exec_windows(&[&followed]);
        assert_eq!(windows.len(), 1, "only the closed second call: {windows:?}");
        assert_eq!(windows[0].source.call, 1);
    }

    /// Ein Fenster: der Aufruf. Mehrere: nur, wenn `prefer` genau einen
    /// übrig lässt — sonst keiner.
    #[test]
    fn a_frame_is_attributed_to_exactly_one_exec_call_or_none() {
        let at = |s: &str| -> Timestamp { s.parse().unwrap() };
        let source = |call: usize| LineSource {
            session: SessionId::from_canonical_bytes(b"session"),
            turn: 0,
            call,
        };
        let window = |call: usize, from: &str, to: &str| ExecWindow {
            source: source(call),
            from: at(from),
            to: at(to),
        };
        let windows = [
            window(0, "2026-10-05T10:00:00Z", "2026-10-05T10:00:20Z"),
            window(1, "2026-10-05T10:00:10Z", "2026-10-05T10:00:30Z"),
            window(2, "2026-10-05T10:01:00Z", "2026-10-05T10:01:05Z"),
        ];
        let none = |_: LineSource| false;
        // Außerhalb jedes Fensters.
        assert_eq!(
            exec_call_at(&windows, at("2026-10-05T10:00:45Z"), &none),
            None
        );
        // Genau eines (Ränder eingeschlossen).
        assert_eq!(
            exec_call_at(&windows, at("2026-10-05T10:00:05Z"), &none),
            Some(source(0))
        );
        assert_eq!(
            exec_call_at(&windows, at("2026-10-05T10:01:05Z"), &none),
            Some(source(2))
        );
        // Zwei überlappende: ohne Vorzug keiner, mit eindeutigem Vorzug der.
        let overlap = at("2026-10-05T10:00:15Z");
        assert_eq!(exec_call_at(&windows, overlap, &none), None);
        assert_eq!(
            exec_call_at(&windows, overlap, &|s| s.call == 1),
            Some(source(1))
        );
        assert_eq!(exec_call_at(&windows, overlap, &|_| true), None);
    }

    #[test]
    fn the_session_window_spans_all_known_times_plus_slack() {
        use minds_core::{Agent, Intent, Model, Role, Turn};
        let mut session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "m".into(),
            },
            Intent::default(),
        );
        assert_eq!(session_window(&[&session]), None);
        for at in ["2026-10-05T10:00:10Z", "2026-10-05T10:05:00Z"] {
            session.turns.push(Turn {
                role: Role::User,
                text: String::new(),
                tool_calls: Vec::new(),
                parent: None,
                at: Some(at.into()),
            });
        }
        let (from, to) = session_window(&[&session]).unwrap();
        assert_eq!(from.to_string(), "2026-10-05T09:59:40Z");
        assert_eq!(to.to_string(), "2026-10-05T10:05:30Z");
    }
}
