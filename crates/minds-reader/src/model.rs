//! Das Sichtmodell: was eine Oberfläche über Sessions, Änderungen und ihre
//! Herkunft zeigt — als reine Daten, ohne I/O und ohne Widget.
//!
//! Der Schnitt folgt dem Crate-Prinzip „I/O dünn, Logik rein": CLI und TUI
//! rendern dieselben Typen, keiner von beiden liest selbst Git oder Store.
//! Alle Strings hier sind bereits durch [`crate::sanitize`] gegangen — wer sie
//! in ein Terminal schreibt, muss nichts mehr entschärfen.
//!
//! # Evidenz ist Teil des Modells
//!
//! Minds ist ein Herkunftssystem. Eine heuristisch vermutete Verknüpfung
//! darf in keiner Oberfläche wie ein Beleg aussehen — deshalb trägt jede
//! Kante hier ihre [`Evidence`] und jede Erklärung sagt, ob sie aus einem
//! Trailer stammt oder nachgerechnet wurde ([`EvidenceExplanation`]).

use minds_core::{
    ChangeId, Decision, EvidenceMark, EvidenceSource, EvidenceStatus, SessionId, Subject,
};
use minds_git::CommitId;
use minds_metrics::Coverage;

use crate::index::Degradation;
use crate::summary::Summary;

/// Die Kopfzeile einer Übersicht: Repository und Kennzahlen.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// Der Name des Repositories (Verzeichnisname), entschärft.
    pub repo: String,
    /// Der aktuelle Branch; `None` bei losgelöstem HEAD.
    pub branch: Option<String>,
    /// Wie viele Sessions gezeigt werden.
    pub sessions: usize,
    /// Wie viele verschiedene Change-Ids die Historie trägt.
    pub changes: usize,
    /// Wie viele gelistete Sessions nicht zeigbar sind (vergessen, defekt).
    pub degraded: usize,
    /// Die Abdeckung der Historie mit Kontext.
    pub coverage: Coverage,
}

/// Ob eine Karte eine lesbare Session zeigt oder nur ihren Platzhalter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardState {
    /// Lesbar und redigiert.
    Ok,
    /// Getilgt per `minds forget`.
    Forgotten {
        /// Der hinterlegte Grund, entschärft.
        reason: String,
    },
    /// Gelistet, aber nicht lesbar.
    Unreadable {
        /// Warum.
        cause: Degradation,
    },
}

/// Eine Session, wie sie in einer Liste steht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCard {
    /// Die Id der Session.
    pub id: SessionId,
    /// Überschrift, Akteur, Umfang — verdichtet aus der Session.
    pub summary: Summary,
    /// Startzeitpunkt als RFC-3339-Text, falls erfasst.
    pub started_at: Option<String>,
    /// Startzeitpunkt in Sekunden seit Epoch — für die Sortierung. `None`,
    /// wenn nicht erfasst oder nicht lesbar.
    pub epoch: Option<i64>,
    /// Der beste Beleg, mit dem die Session an Code hängt; `None`, wenn sie
    /// mit keinem Commit verbunden ist.
    pub evidence: Option<EvidenceMark>,
    /// Die Herkunftslage: Legacy oder versiegelt, mit Verdikt (ADR-0011).
    pub provenance: Provenance,
    /// Tool-Aufrufe, die beobachtet, aber nicht gedeutet sind (`◐`) — die
    /// Deutungs-Achse, getrennt von Integrität und Coverage.
    pub uninterpreted_calls: usize,
    /// Die Epochen-Position `(k, n)` in der Seal-Kette; `None` bei einer
    /// einzelnen Epoche.
    pub epoch_position: Option<(usize, usize)>,
    /// Content-Übergaben, an denen die Session beteiligt ist (Evidence-DAG).
    pub handovers: usize,
    /// Der Stand des Reviews.
    pub review: ReviewState,
    /// Die Change-Ids der Commits, die die Session tragen.
    pub changes: Vec<ChangeId>,
    /// Die Commits, die die Session tragen.
    pub commits: Vec<CommitId>,
    /// Von dieser Session gestartete Sub-Agents, soweit im Index auflösbar.
    pub subagents: Vec<SessionId>,
    /// Die Session, die diese gestartet hat, soweit auflösbar.
    pub parent: Option<SessionId>,
    /// Lesbar oder degradiert.
    pub state: CardState,
}

impl SessionCard {
    /// `true`, wenn die Karte nur ein Platzhalter ist.
    pub fn is_degraded(&self) -> bool {
        !matches!(self.state, CardState::Ok)
    }

    /// Das Seal-Verdikt, falls versiegelt — Kurzform für Anzeigen.
    pub fn evidence_state(&self) -> Option<&EvidenceState> {
        self.provenance.state()
    }
}

/// Das Verdict, auf einen Blick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Kein Review vorhanden.
    Open,
    /// Angenommen.
    Approved,
    /// Abgelehnt.
    Rejected,
    /// Nacharbeit nötig.
    NeedsWork,
}

impl Verdict {
    /// Das Verdict zu einer Entscheidung.
    pub fn of(decision: Decision) -> Self {
        match decision {
            Decision::Approve => Verdict::Approved,
            Decision::Reject => Verdict::Rejected,
            Decision::NeedsWork => Verdict::NeedsWork,
        }
    }

    /// Das Wort für die Anzeige — neben der Farbe, nie nur die Farbe.
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::Open => "offen",
            Verdict::Approved => "approved",
            Verdict::Rejected => "rejected",
            Verdict::NeedsWork => "needs work",
        }
    }
}

/// Ein einzelnes Review, wie es gezeigt wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNote {
    /// Woran das Review hängt.
    pub subject: Subject,
    /// Die Entscheidung.
    pub decision: Decision,
    /// Wer entschieden hat, entschärft.
    pub reviewer: String,
    /// Die Begründung, entschärft.
    pub summary: String,
    /// Wann, als RFC-3339-Text.
    pub at: Option<String>,
    /// Ob eine Signatur **vorliegt** — nicht, ob sie gültig ist; das prüft
    /// `minds verify`.
    pub signed: bool,
}

/// Der Review-Stand eines Subjekts.
///
/// Das Verdict ist das **jüngste** Review (nach Zeitstempel, bei Gleichstand
/// nach Inhalt-Hash — deterministisch); die Notizen stehen vollständig
/// dabei, damit niemand einen Widerspruch übersieht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewState {
    /// Das geltende Verdict.
    pub verdict: Verdict,
    /// Alle Reviews, älteste zuerst.
    pub notes: Vec<ReviewNote>,
}

impl ReviewState {
    /// Kein Review.
    pub fn open() -> Self {
        Self {
            verdict: Verdict::Open,
            notes: Vec::new(),
        }
    }
}

impl Default for ReviewState {
    fn default() -> Self {
        Self::open()
    }
}

/// Das Evidence-Verdikt einer Session — dieselbe Matrix wie `minds verify`
/// (ADR-0011, Entscheidung 7), fürs Lesemodell verdichtet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceVerdict {
    /// Integrität intakt, Coverage vollständig.
    Verified,
    /// Seal-Material wurde verändert.
    Tampered,
    /// Intakt, aber Lücken, offene Epochen oder eine zurückgewiesene
    /// Nutzlast.
    Incomplete,
}

impl EvidenceVerdict {
    /// Das Wort der Matrix.
    pub fn word(self) -> &'static str {
        match self {
            EvidenceVerdict::Verified => "VERIFIZIERT",
            EvidenceVerdict::Tampered => "MANIPULIERT",
            EvidenceVerdict::Incomplete => "VERIFIZIERT, UNVOLLSTÄNDIG",
        }
    }
}

/// Was die Seals einer Session über sie aussagen, verdichtet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceState {
    /// Das Verdikt.
    pub verdict: EvidenceVerdict,
    /// Zahl der Seals (Epochen).
    pub seals: usize,
    /// Versiegelte Events über alle Epochen.
    pub events: u64,
    /// Versiegelte Lücken über alle Epochen.
    pub gaps: u64,
    /// Events ohne Stempel (vor Evidence-Chain erfasst).
    pub pre_chain: u64,
    /// Die Kette führt über einen Block-Seal — eine frühere Epoche wurde
    /// von der Speicher-Policy zurückgewiesen.
    pub rejected: bool,
    /// Die `previous`-Kette der Epochen schließt sich.
    pub chain_closed: bool,
    /// Wie viele Seals eine Signatur tragen (Anwesenheit, hier ungeprüft).
    pub signed: usize,
}

impl EvidenceState {
    /// Die Kurzform für Listen: `2 Seals · 48 Events · 0 Lücken`.
    pub fn summary(&self) -> String {
        format!(
            "{} Seal(s) · {} Event(s) · {} Lücke(n)",
            self.seals, self.events, self.gaps
        )
    }
}

/// Die Herkunftslage einer Session — ein expliziter Zustand, kein `None`.
///
/// `None` wäre semantisch zu schwach: Es könnte „alte Session", „kaputt",
/// „noch nicht verarbeitet" oder „Bug" heißen. Deshalb ein eigener Typ
/// (Invariante: **Legacy bleibt Legacy** — eine Session ohne Chain bekommt
/// nie nachträglich eine angedichtet; ihre ehrliche Auskunft ist `Legacy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Vor der Evidence-Chain erfasst: kein Seal-Material. Kein Mangel,
    /// ein historischer Zustand.
    Legacy,
    /// Versiegelt — mit dem Verdikt und der Coverage aus den Seals.
    Chained(EvidenceState),
}

impl Provenance {
    /// Das Verdikt, falls versiegelt.
    pub fn state(&self) -> Option<&EvidenceState> {
        match self {
            Provenance::Legacy => None,
            Provenance::Chained(state) => Some(state),
        }
    }
}

/// Warum eine Kante Commit ↔ Session im Index steht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceExplanation {
    /// Beobachtet: Der Commit trägt den Trailer `Minds-Session-Id`.
    Trailer {
        /// Der Commit mit dem Trailer.
        commit: CommitId,
    },
    /// Ein Mensch hat es behauptet (`--after`).
    Declared,
    /// Nachrechenbar über den Inhalt: gelesene Bytes sind geschriebene.
    Content,
    /// Vermutet: Datei-Schnittmenge und Zeitfenster. Die Gründe sind
    /// **nachgerechnet**, nicht protokolliert — der Import hat sie nicht
    /// gespeichert.
    Heuristic {
        /// Dateien, die Session und Commit gemeinsam berühren, entschärft.
        shared_files: Vec<String>,
        /// Abstand des Commits zum Session-Ende in Sekunden (negativ: der
        /// Commit liegt vor dem Ende). `None`, wenn eine Zeit fehlt.
        seconds_apart: Option<i64>,
        /// Ob der Commit im Karenzfenster der Heuristik liegt; `None`, wenn
        /// sich das mangels Zeiten nicht sagen lässt.
        in_window: Option<bool>,
    },
    /// Die Kante steht im Index, ihre Gründe ließen sich nicht rekonstruieren
    /// (etwa weil der Commit-Diff nicht lesbar ist).
    Unknown {
        /// Warum nicht, entschärft.
        reason: String,
    },
}

/// Eine erklärte Kante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEvidence {
    /// Der Commit.
    pub commit: CommitId,
    /// Die Session.
    pub session: SessionId,
    /// Der Beleg.
    pub evidence: EvidenceMark,
    /// Die Erklärung.
    pub why: EvidenceExplanation,
}

/// Die Herkunftskette — von der Zeile zum Intent und zurück zur Bewertung.
///
/// Fehlende Glieder werden als solche geführt, nie verschwiegen: `Change {
/// id: None }` heißt „dieser Commit trägt keine Change-Id", nicht „es gibt
/// keinen Schritt".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyChain {
    /// Die Glieder, von außen (Code) nach innen (Review).
    pub steps: Vec<WhyStep>,
}

/// Ein Glied der Herkunftskette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhyStep {
    /// Die Zeile, nach der gefragt wurde.
    Line {
        /// Der Pfad, entschärft.
        path: String,
        /// Die Zeile, 1-basiert.
        line: u32,
    },
    /// Der Commit hinter der Zeile (Blame). `id: None`: Blame kennt die
    /// Zeile nicht (leerer HEAD, nicht eingecheckt).
    Commit {
        /// Der Commit.
        id: Option<CommitId>,
        /// Der Betreff, entschärft.
        subject: Option<String>,
    },
    /// Die Change-Id des Commits. `None`: kein `Minds-Change-Id`-Trailer.
    Change {
        /// Die Change-Id.
        id: Option<ChangeId>,
    },
    /// Die Session(s) hinter dem Commit. Leer: kein Kontext erfasst.
    Sessions {
        /// Die Karten, in Index-Reihenfolge.
        cards: Vec<SessionCard>,
    },
    /// Der Agent der (ersten) Session.
    Agent {
        /// Name, entschärft.
        name: String,
        /// Version, entschärft.
        version: String,
        /// Modell-Id, entschärft.
        model: String,
    },
    /// Die Absicht.
    Intent {
        /// Der Prompt, entschärft.
        request: String,
        /// Die Constraints, entschärft.
        constraints: Vec<String>,
        /// Verworfene Wege, entschärft.
        discarded: Vec<String>,
    },
    /// Womit die Kante belegt ist.
    Evidence {
        /// Je Commit-Session-Paar ein Beleg.
        links: Vec<LinkEvidence>,
    },
    /// Die Bewertung.
    Review {
        /// Der Stand.
        state: ReviewState,
    },
}

/// Was eine Evidenz-Klasse bedeutet — der Satz, der neben Glyph und Wort
/// steht, damit niemand nur lernt „○ heißt irgendwie unsicher", sondern
/// **warum**. Seit ADR-0011 hat der Satz zwei Hälften: die **Quelle** (woher
/// die Aussage stammt) und den **Status** (ob sie je geprüft wurde) — denn
/// „beobachtet" klingt sicherer als „vermutet", sagt aber nichts darüber, ob
/// jemand nachgerechnet hat.
pub fn evidence_sentence(evidence: Option<EvidenceMark>) -> String {
    let Some(mark) = evidence else {
        return "Unverknüpft: Diese Session hängt an keinem Commit — erfasst, aber (noch) nicht mit Code verbunden.".into();
    };
    let source = match mark.source {
        EvidenceSource::Observed => {
            "Beobachtet: Der Commit trägt den Trailer Minds-Session-Id — ein expliziter Herkunftsnachweis."
        }
        EvidenceSource::ContentDerived => {
            "Inhaltlich: Die gelesenen Bytes sind die geschriebenen — kein Zeitstempel nötig."
        }
        EvidenceSource::HumanDeclared => {
            "Erklärt: Ein Mensch hat die Verbindung behauptet (--after) — eine Tatsache über den Menschen, nicht über den Code."
        }
        EvidenceSource::Heuristic => {
            "Vermutet: Von Minds rekonstruiert aus Datei-Überschneidung und zeitlicher Nähe — es gibt keinen expliziten Herkunftsnachweis."
        }
    };
    let status = match mark.status {
        EvidenceStatus::Verified => " Status: nachgerechnet und bestanden.",
        EvidenceStatus::Partial => " Status: teilweise nachgerechnet.",
        EvidenceStatus::Unknown => " Status: nie nachgerechnet — beobachtet heißt nicht geprüft.",
        EvidenceStatus::Missing => {
            " Status: der Beleg müsste existieren, ist aber nicht auffindbar."
        }
    };
    format!("{source}{status}")
}

/// Welche Art von Lücke eine Herkunftskette hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    /// Blame kennt die Zeile nicht.
    NoCommit,
    /// Der Commit trägt keine Change-Id.
    NoChangeId,
    /// Kein Kontext zum Commit erfasst.
    NoContext,
    /// Die Zuordnung Session ↔ Commit ist nur vermutet.
    InferredAttribution,
    /// Eine Session ist vergessen oder unlesbar.
    DegradedContext,
    /// Niemand hat die Änderung bewertet.
    NoReview,
    /// Kryptographisch versiegelte Sequenz-Lücken: Im Beobachtungsbereich
    /// fehlen Events, und die Chain beweist es (ADR-0011).
    SealedGap,
    /// Der Beobachtungsbereich der Session ist nicht (vollständig)
    /// versiegelt — vor Evidence-Chain erfasst oder offene Epochenkette.
    UnsealedRange,
    /// Eine Epoche der Session wurde von der Speicher-Policy zurückgewiesen;
    /// der Block-Seal ist der Beleg.
    PayloadRejected,
}

/// Eine benannte Lücke in der Kette — eine der wichtigsten Aussagen des
/// Systems, deshalb ein eigener Typ statt eines `None` irgendwo im Glied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// Der Index des Glieds in [`WhyChain::steps`].
    pub step: usize,
    /// Die Art.
    pub kind: GapKind,
    /// Die Begründung, entschärft, für Menschen.
    pub text: String,
}

impl WhyChain {
    /// Die Lücken der Kette, in Kettenreihenfolge.
    ///
    /// Eine Vermutung ist eine Lücke: Die Kette schließt sich dann nur über
    /// Rekonstruktion, nicht über einen Nachweis. Ein fehlendes Review ist
    /// ebenfalls eine — die Kette endet bei der Bewertung, und ohne sie hat
    /// niemand die Änderung entschieden.
    pub fn gaps(&self) -> Vec<Gap> {
        let mut out = Vec::new();
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                WhyStep::Commit { id: None, .. } => out.push(Gap {
                    step: i,
                    kind: GapKind::NoCommit,
                    text: "Blame kennt die Zeile nicht — ohne Commit keine Herkunft.".into(),
                }),
                WhyStep::Change { id: None } => out.push(Gap {
                    step: i,
                    kind: GapKind::NoChangeId,
                    text: "Der Commit trägt keine Minds-Change-Id — Reviews können nur an der Session hängen, nicht an der Änderung."
                        .into(),
                }),
                WhyStep::Sessions { cards } if cards.is_empty() => out.push(Gap {
                    step: i,
                    kind: GapKind::NoContext,
                    text: "Kein Kontext erfasst — zu diesem Commit gibt es keine Session, die Absicht ist nicht nachlesbar."
                        .into(),
                }),
                WhyStep::Sessions { cards } => {
                    for card in cards.iter().filter(|c| !c.is_degraded()) {
                        let short: String = card.id.to_string().chars().take(11).collect();
                        match &card.provenance {
                            Provenance::Legacy => out.push(Gap {
                                step: i,
                                kind: GapKind::UnsealedRange,
                                text: format!(
                                    "Session {short}… ist nicht versiegelt — vor der Evidence-Chain erfasst; \
                                     der Beobachtungsbereich ist nicht belegt."
                                ),
                            }),
                            Provenance::Chained(state) => {
                                if state.gaps > 0 {
                                    out.push(Gap {
                                        step: i,
                                        kind: GapKind::SealedGap,
                                        text: format!(
                                            "Session {short}…: {} Sequenz-Lücke(n), kryptographisch versiegelt — \
                                             im Beobachtungsbereich fehlen Events. Fehlende Evidence beweist nicht, \
                                             dass nichts geschah.",
                                            state.gaps
                                        ),
                                    });
                                }
                                if state.rejected {
                                    out.push(Gap {
                                        step: i,
                                        kind: GapKind::PayloadRejected,
                                        text: format!(
                                            "Session {short}…: eine frühere Epoche wurde von der Speicher-Policy \
                                             zurückgewiesen — der Block-Seal ist der Beleg, die Nutzlast fehlt."
                                        ),
                                    });
                                }
                                if !state.chain_closed || state.pre_chain > 0 {
                                    out.push(Gap {
                                        step: i,
                                        kind: GapKind::UnsealedRange,
                                        text: format!(
                                            "Session {short}…: der Beobachtungsbereich ist nicht vollständig \
                                             versiegelt ({}).",
                                            if state.pre_chain > 0 {
                                                format!("{} Event(s) ohne Stempel", state.pre_chain)
                                            } else {
                                                "Epochenkette offen".to_string()
                                            }
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    for card in cards.iter().filter(|c| c.is_degraded()) {
                        out.push(Gap {
                            step: i,
                            kind: GapKind::DegradedContext,
                            text: format!(
                                "Session {}… ist {} — ihre Absicht ist nicht mehr nachlesbar.",
                                card.id.to_string().chars().take(11).collect::<String>(),
                                match &card.state {
                                    CardState::Forgotten { reason } => format!("vergessen ({reason})"),
                                    _ => "unlesbar".to_string(),
                                }
                            ),
                        });
                    }
                }
                WhyStep::Evidence { links } if !links.is_empty() => {
                    let best = links.iter().map(|l| l.evidence).max();
                    if best.map(|m| m.source) == Some(EvidenceSource::Heuristic) {
                        let detail = links
                            .iter()
                            .find_map(|l| match &l.why {
                                EvidenceExplanation::Heuristic {
                                    shared_files,
                                    seconds_apart,
                                    ..
                                } => Some(format!(
                                    " Nachgerechnet: {} gemeinsame Datei(en){}.",
                                    shared_files.len(),
                                    seconds_apart
                                        .map(|s| format!(", {s} s zwischen Session-Ende und Commit"))
                                        .unwrap_or_default()
                                )),
                                _ => None,
                            })
                            .unwrap_or_default();
                        out.push(Gap {
                            step: i,
                            kind: GapKind::InferredAttribution,
                            text: format!(
                                "Die Zuordnung Session ↔ Commit ist rekonstruiert aus Datei-Überschneidung und zeitlicher Nähe — kein expliziter Herkunftsnachweis.{detail}"
                            ),
                        });
                    }
                }
                WhyStep::Review { state } if state.verdict == Verdict::Open => out.push(Gap {
                    step: i,
                    kind: GapKind::NoReview,
                    text: "Keine Bewertung — niemand hat diese Änderung entschieden.".into(),
                }),
                _ => {}
            }
        }
        out
    }

    /// `true`, wenn an diesem Glied eine Lücke hängt.
    pub fn is_gap(&self, step: usize) -> bool {
        self.gaps().iter().any(|g| g.step == step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_with_state(state: Option<EvidenceState>) -> SessionCard {
        let id: SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        SessionCard {
            id,
            summary: crate::summary::Summary {
                id,
                headline: "x".into(),
                actor: "a".into(),
                files: 0,
                constraints: 0,
                discarded: 0,
                input_tokens: 0,
                output_tokens: 0,
            },
            started_at: None,
            epoch: None,
            evidence: None,
            provenance: match state {
                Some(state) => Provenance::Chained(state),
                None => Provenance::Legacy,
            },
            uninterpreted_calls: 0,
            epoch_position: None,
            handovers: 0,
            review: ReviewState::open(),
            changes: Vec::new(),
            commits: Vec::new(),
            subagents: Vec::new(),
            parent: None,
            state: CardState::Ok,
        }
    }

    fn chain_with(card: SessionCard) -> WhyChain {
        WhyChain {
            steps: vec![WhyStep::Sessions { cards: vec![card] }],
        }
    }

    fn clean_state() -> EvidenceState {
        EvidenceState {
            verdict: EvidenceVerdict::Verified,
            seals: 1,
            events: 4,
            gaps: 0,
            pre_chain: 0,
            rejected: false,
            chain_closed: true,
            signed: 0,
        }
    }

    #[test]
    fn an_unsealed_session_is_an_honest_gap_not_silence() {
        let gaps = chain_with(card_with_state(None)).gaps();
        assert!(
            gaps.iter().any(|g| g.kind == GapKind::UnsealedRange),
            "{gaps:?}"
        );
        assert!(gaps[0].text.contains("nicht versiegelt"), "{gaps:?}");
    }

    #[test]
    fn a_sealed_gap_carries_the_honesty_sentence() {
        let state = EvidenceState {
            verdict: EvidenceVerdict::Incomplete,
            gaps: 2,
            ..clean_state()
        };
        let gaps = chain_with(card_with_state(Some(state))).gaps();
        let sealed = gaps
            .iter()
            .find(|g| g.kind == GapKind::SealedGap)
            .expect("SealedGap");
        // Der Satz, der das ganze System traegt: Abwesenheit von Evidence
        // beweist nicht Abwesenheit des Ereignisses.
        assert!(
            sealed
                .text
                .contains("Fehlende Evidence beweist nicht, dass nichts geschah"),
            "{}",
            sealed.text
        );
    }

    #[test]
    fn a_rejected_epoch_and_an_open_chain_are_named() {
        let state = EvidenceState {
            verdict: EvidenceVerdict::Incomplete,
            rejected: true,
            chain_closed: false,
            ..clean_state()
        };
        let gaps = chain_with(card_with_state(Some(state))).gaps();
        assert!(
            gaps.iter().any(|g| g.kind == GapKind::PayloadRejected),
            "{gaps:?}"
        );
        assert!(
            gaps.iter().any(|g| g.kind == GapKind::UnsealedRange),
            "{gaps:?}"
        );
    }

    #[test]
    fn a_cleanly_sealed_session_adds_no_gap() {
        let gaps = chain_with(card_with_state(Some(clean_state()))).gaps();
        assert!(gaps.is_empty(), "{gaps:?}");
    }

    fn commit() -> CommitId {
        "1".repeat(40).parse().unwrap()
    }

    fn sid() -> SessionId {
        format!("b3-{}", "a".repeat(64)).parse().unwrap()
    }

    fn chain(steps: Vec<WhyStep>) -> WhyChain {
        WhyChain { steps }
    }

    #[test]
    fn a_line_blame_cannot_resolve_is_one_gap() {
        let c = chain(vec![
            WhyStep::Line {
                path: "a.rs".into(),
                line: 1,
            },
            WhyStep::Commit {
                id: None,
                subject: None,
            },
        ]);
        let gaps = c.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::NoCommit);
        assert_eq!(gaps[0].step, 1);
        assert!(!c.is_gap(0));
        assert!(c.is_gap(1));
    }

    #[test]
    fn a_commit_without_context_names_change_context_and_review_gaps() {
        let c = chain(vec![
            WhyStep::Commit {
                id: Some(commit()),
                subject: None,
            },
            WhyStep::Change { id: None },
            WhyStep::Sessions { cards: vec![] },
            WhyStep::Evidence { links: vec![] },
            WhyStep::Review {
                state: ReviewState::open(),
            },
        ]);
        let kinds: Vec<GapKind> = c.gaps().iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            vec![GapKind::NoChangeId, GapKind::NoContext, GapKind::NoReview]
        );
    }

    #[test]
    fn an_inferred_attribution_is_a_gap_with_its_reconstruction() {
        let c = chain(vec![WhyStep::Evidence {
            links: vec![LinkEvidence {
                commit: commit(),
                session: sid(),
                evidence: EvidenceMark::of(EvidenceSource::Heuristic),
                why: EvidenceExplanation::Heuristic {
                    shared_files: vec!["a.rs".into(), "b.rs".into()],
                    seconds_apart: Some(287),
                    in_window: Some(true),
                },
            }],
        }]);
        let gaps = c.gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::InferredAttribution);
        assert!(
            gaps[0].text.contains("2 gemeinsame Datei(en)"),
            "{}",
            gaps[0].text
        );
        assert!(gaps[0].text.contains("287 s"), "{}", gaps[0].text);
        assert!(gaps[0].text.contains("kein expliziter Herkunftsnachweis"));
    }

    #[test]
    fn an_observed_edge_is_no_gap() {
        let c = chain(vec![WhyStep::Evidence {
            links: vec![LinkEvidence {
                commit: commit(),
                session: sid(),
                evidence: EvidenceMark::of(EvidenceSource::Observed),
                why: EvidenceExplanation::Trailer { commit: commit() },
            }],
        }]);
        assert!(c.gaps().is_empty());
    }

    #[test]
    fn every_evidence_class_has_a_sentence_that_says_why() {
        for ev in [
            None,
            Some(EvidenceMark::of(EvidenceSource::Heuristic)),
            Some(EvidenceMark::of(EvidenceSource::HumanDeclared)),
            Some(EvidenceMark::of(EvidenceSource::ContentDerived)),
            Some(EvidenceMark::of(EvidenceSource::Observed)),
        ] {
            assert!(evidence_sentence(ev).contains(':'));
        }
        assert!(
            evidence_sentence(Some(EvidenceMark::of(EvidenceSource::Heuristic)))
                .contains("keinen expliziten Herkunftsnachweis")
        );
    }
}
