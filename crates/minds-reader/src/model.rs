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

use minds_core::{ChangeId, Decision, Evidence, SessionId, Subject};
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
    pub evidence: Option<Evidence>,
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
    pub evidence: Evidence,
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
/// **warum**.
pub fn evidence_sentence(evidence: Option<Evidence>) -> &'static str {
    match evidence {
        Some(Evidence::Observed) => {
            "Beobachtet: Der Commit trägt den Trailer Minds-Session-Id — ein expliziter Herkunftsnachweis."
        }
        Some(Evidence::Content) => {
            "Nachgerechnet über den Inhalt: Die gelesenen Bytes sind die geschriebenen — kein Zeitstempel nötig."
        }
        Some(Evidence::Declared) => {
            "Erklärt: Ein Mensch hat die Verbindung behauptet (--after) — eine Tatsache über den Menschen, nicht über den Code."
        }
        Some(Evidence::Inferred) => {
            "Vermutet: Von Minds rekonstruiert aus Datei-Überschneidung und zeitlicher Nähe — es gibt keinen expliziten Herkunftsnachweis."
        }
        None => {
            "Unverknüpft: Diese Session hängt an keinem Commit — erfasst, aber (noch) nicht mit Code verbunden."
        }
    }
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
                    if best == Some(Evidence::Inferred) {
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
                evidence: Evidence::Inferred,
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
                evidence: Evidence::Observed,
                why: EvidenceExplanation::Trailer { commit: commit() },
            }],
        }]);
        assert!(c.gaps().is_empty());
    }

    #[test]
    fn every_evidence_class_has_a_sentence_that_says_why() {
        for ev in [
            None,
            Some(Evidence::Inferred),
            Some(Evidence::Declared),
            Some(Evidence::Content),
            Some(Evidence::Observed),
        ] {
            assert!(evidence_sentence(ev).contains(':'));
        }
        assert!(
            evidence_sentence(Some(Evidence::Inferred))
                .contains("keinen expliziten Herkunftsnachweis")
        );
    }
}
