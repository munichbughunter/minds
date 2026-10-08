//! Erstsicht-Gegenzeichnungen zur Lesezeit (EA-19).
//!
//! `minds anchor` legt je Seal einen Text `minds-anchor-v1` samt Signatur
//! unter `refs/minds/anchors/first-sight/` ab und spiegelt ihn als MR-Note
//! nach GitLab. Was davon **gilt**, wird hier gerechnet (W2/W5) — aus dem,
//! was der Store roh liefert, und einem Signatur-Prädikat des Aufrufers:
//!
//! - Eine Gegenzeichnung zählt nur, wenn ihr Text parst, genau diesen Seal
//!   nennt und die Signatur unter `minds-anchor` von einem vertrauenswürdigen
//!   Principal trägt ([`AnchorSignature::Valid`]).
//! - Eine gespiegelte Note beweist das Fehlen eines Refs nur, wenn ihr
//!   Eintrag selbst gültig signiert ist — eine Note kann jeder schreiben,
//!   der den Merge Request kommentieren darf ([`note_findings`]).

use std::collections::BTreeSet;

use minds_core::ContentHash;
use minds_core::first_sight::FirstSight;
use minds_store::FirstSightRef;

use crate::assurance::AnchorSummary;

/// Die Signaturlage einer Gegenzeichnung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSignature {
    /// Unter `minds-anchor` von einem Principal, der auf diesen Namespace
    /// beschränkt ist, gültig.
    Valid,
    /// Ohne vertrauenswürdige Signer nicht prüfbar.
    NotChecked,
    /// Geprüft und ungültig (falscher Namespace, fremder oder
    /// unbeschränkter Schlüssel, veränderter Text).
    Invalid,
}

/// Was über die Gegenzeichnung eines Seals sagbar ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstSightState {
    /// Kein Ref.
    Absent,
    /// Ein lesbarer Text, der diesen Seal nennt, samt Signaturlage.
    Anchored {
        /// Die geparste Gegenzeichnung.
        anchor: FirstSight,
        /// Die Signaturlage.
        signature: AnchorSignature,
    },
    /// Der Ref ist belegt, trägt aber keine lesbare Gegenzeichnung dieses
    /// Seals (kein Text, kein `minds-anchor-v1`, ein anderer Seal, keine
    /// armierte Signatur). Zählt nie.
    Unreadable,
}

impl FirstSightState {
    /// Zählt die Gegenzeichnung (A3, `anchored:`)?
    pub fn valid(&self) -> bool {
        matches!(
            self,
            Self::Anchored {
                signature: AnchorSignature::Valid,
                ..
            }
        )
    }
}

/// Das Signatur-Prädikat: `None` ohne vertrauenswürdige Signer, sonst ob
/// `signature` den Text `text` unter `minds-anchor` trägt.
pub type CheckAnchor<'a> = dyn Fn(&str, &str) -> Option<bool> + 'a;

/// Die Lage der Gegenzeichnung von `seal` aus dem rohen Ref-Inhalt.
pub fn state_of(
    seal: &ContentHash,
    raw: &FirstSightRef,
    check: &CheckAnchor<'_>,
) -> FirstSightState {
    let FirstSightRef::Present { text, signature } = raw else {
        return FirstSightState::Absent;
    };
    let (Some(text), Some(signature)) = (text, signature) else {
        return FirstSightState::Unreadable;
    };
    let Ok(anchor) = FirstSight::parse(text) else {
        return FirstSightState::Unreadable;
    };
    // Ein Text unter dem Ref eines anderen Seals bezeugt nichts über diesen.
    if anchor.seal != *seal {
        return FirstSightState::Unreadable;
    }
    let signature = match check(text, signature) {
        None => AnchorSignature::NotChecked,
        Some(true) => AnchorSignature::Valid,
        Some(false) => AnchorSignature::Invalid,
    };
    FirstSightState::Anchored { anchor, signature }
}

/// Die gültig gegengezeichneten Seals — der Eingang
/// [`crate::assurance::AssuranceInput::anchors`].
pub fn summary<'a>(
    states: impl IntoIterator<Item = (&'a ContentHash, &'a FirstSightState)>,
) -> AnchorSummary {
    AnchorSummary {
        anchored: states
            .into_iter()
            .filter(|(_, state)| state.valid())
            .map(|(seal, _)| seal.clone())
            .collect(),
    }
}

/// Ein Eintrag einer gespiegelten MR-Note: Text und Signatur, roh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEntry {
    /// Die Textform, wie sie in der Note stand.
    pub text: String,
    /// Die armierte Signatur, wie sie in der Note stand.
    pub signature: String,
    /// Stammt die Note aus einem gemergten Merge Request? Nur dort schreibt
    /// `minds anchor --mirror`; einen offenen kann jeder anlegen und echte
    /// Einträge hineinkopieren.
    pub merged: bool,
}

/// So viele Einträge je Seal werden höchstens geprüft — jede Prüfung
/// startet `ssh-keygen`, und Notes kann jeder schreiben.
pub const MAX_CHECKS_PER_SEAL: usize = 4;

/// So viele Prüfungen macht ein Abgleich insgesamt höchstens.
pub const MAX_CHECKS_TOTAL: usize = 256;

/// Was der Abgleich der MR-Notes mit den Refs ergibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteFinding {
    /// Ein gültig signierter Eintrag nennt einen Seal der Session, sein Ref
    /// fehlt — **Integritätsbefund**: gelöscht (oder nie gepusht).
    RefMissing {
        /// Der Seal.
        seal: ContentHash,
        /// Die Pipeline laut Note.
        pipeline: u64,
    },
    /// Ein gültig signierter Eintrag nennt einen Seal der Session, der Ref
    /// trägt aber keine gültige Gegenzeichnung — **Integritätsbefund**:
    /// ersetzt.
    RefAltered {
        /// Der Seal.
        seal: ContentHash,
        /// Die Pipeline laut Note.
        pipeline: u64,
    },
    /// Ein gültig signierter Eintrag nennt einen Seal, den es im Repository
    /// nicht (mehr) gibt — **Integritätsbefund**: Wer Seal und Anker
    /// zusammen löscht, soll nicht besser fahren als mit dem Anker allein.
    SealMissing {
        /// Der Seal.
        seal: ContentHash,
        /// Die Pipeline laut Note.
        pipeline: u64,
    },
    /// Der Ref trägt eine gültige Gegenzeichnung, aber eine gültig
    /// signierte Note nennt eine **frühere** Sicht — **Integritätsbefund**:
    /// Ein Erstsicht-Ref wird nie überschrieben, ein verlorener (atomarer)
    /// Push spiegelt nie, und `--mirror` gibt nur wieder, was im Ref steht.
    /// Eine frühere gültige Sicht neben einem späteren Ref entsteht nur, wenn
    /// der erste Ref gelöscht und der Seal neu gezeichnet wurde.
    RefLater {
        /// Der Seal.
        seal: ContentHash,
        /// Die Pipeline laut Note.
        noted: u64,
        /// Die Pipeline laut Ref.
        stored: u64,
    },
}

impl NoteFinding {
    /// Verletzt der Befund die Integrität?
    pub fn integrity(&self) -> bool {
        matches!(
            self,
            Self::RefMissing { .. }
                | Self::RefAltered { .. }
                | Self::SealMissing { .. }
                | Self::RefLater { .. }
        )
    }

    /// Der Seal des Befunds.
    pub fn seal(&self) -> &ContentHash {
        match self {
            Self::RefMissing { seal, .. }
            | Self::RefAltered { seal, .. }
            | Self::SealMissing { seal, .. }
            | Self::RefLater { seal, .. } => seal,
        }
    }

    /// Integrität vor Hinweis — je Seal bleibt der stärkste.
    fn rank(&self) -> u8 {
        match self {
            Self::RefMissing { .. } | Self::RefAltered { .. } | Self::SealMissing { .. } => 2,
            Self::RefLater { .. } => 1,
        }
    }

    /// Die Befundzeile: `seal b3-…: anchor ref missing, MR note present
    /// (pipeline #N)`.
    pub fn text(&self) -> String {
        match self {
            Self::RefMissing { seal, pipeline } => {
                format!("seal {seal}: anchor ref missing, MR note present (pipeline #{pipeline})")
            }
            Self::RefAltered { seal, pipeline } => {
                format!("seal {seal}: anchor ref altered, MR note present (pipeline #{pipeline})")
            }
            Self::SealMissing { seal, pipeline } => {
                format!("seal {seal}: seal missing, MR anchor note present (pipeline #{pipeline})")
            }
            Self::RefLater {
                seal,
                noted,
                stored,
            } => format!(
                "seal {seal}: anchor ref replaced — the ref names pipeline #{stored}, an MR note \
                 an earlier first sight (pipeline #{noted})"
            ),
        }
    }
}

/// Das Ergebnis des Abgleichs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NoteCheck {
    /// Die Befunde, je Seal höchstens einer, nach Seal sortiert.
    pub findings: Vec<NoteFinding>,
    /// Einträge für Seals dieser Session, die in der Note standen.
    pub mirrored: usize,
    /// Einträge mit **ungültiger** Signatur (oder dem Schlüssel keines
    /// vertrauenswürdigen Signers) — verworfen, nie ein Befund.
    pub forged: usize,
    /// Einträge eines anderen Projekts — nie verglichen. Steht hier mehr
    /// als null und `mirrored` bei null, ist womöglich das falsche Projekt
    /// eingestellt.
    pub other_project: usize,
    /// Gültig signierte Einträge, die einen Seal dieser Session oder einen
    /// fehlenden Seal nennen.
    pub signed: usize,
    /// Die Seals, die Einträge dieses Projekts nennen (geprüft oder nicht).
    pub noted: BTreeSet<ContentHash>,
    /// Einträge, die einen Befund hätten ergeben können, sich aber nicht
    /// als gültig erwiesen (ohne Signer ungeprüft, ungültig, Schlüssel
    /// keines Signers). Steht hier mehr als null und `signed` bei null, ist
    /// der Abgleich nichts wert — der Aufrufer macht daraus einen Fehler.
    pub unvalidated: usize,
    /// Eine Prüfgrenze ([`MAX_CHECKS_PER_SEAL`], [`MAX_CHECKS_TOTAL`]) ließ
    /// einen Eintrag ungeprüft, der noch einen Befund hätte ergeben können
    /// — der Abgleich ist **nicht** vollständig (der Aufrufer macht daraus
    /// einen operativen Fehler, nie „nichts gefunden").
    pub exhausted: bool,
}

/// Gleicht die Einträge der MR-Notes mit den Refs ab, für Einträge des
/// Projekts `project` (eine Note aus einem Fork sagt nichts über dieses
/// Repository):
///
/// - für `seals` (die Seals der geprüften Session) aus jeder Note und für
///   jeden anderen Seal aus Notes **gemergter** Merge Requests (nur dorthin
///   spiegelt `minds anchor --mirror`): Ist der Seal nicht mehr da oder
///   nicht mehr er selbst (`present` heißt: sein Text hasht auf seine Id),
///   [`NoteFinding::SealMissing`]; sonst fehlt oder ersetzt ist der Ref
///   ([`NoteFinding::RefMissing`], [`NoteFinding::RefAltered`]), oder er
///   ist später als die Note ([`NoteFinding::RefLater`]).
///
/// Fail-closed in beide Richtungen: Ein Befund entsteht nur aus einem
/// gültig signierten Eintrag (sonst könnte jeder, der kommentieren darf, ein
/// `TAMPERED` erzeugen); ein ungeprüfter zählt in [`NoteCheck::unvalidated`].
///
/// Gegen Fluten:
///
/// - Gleiche Einträge zählen einmal; geprüft wird in der Reihenfolge der
///   behaupteten Zeit (die früheste Sicht zuerst — gleich, in welcher
///   Reihenfolge die Notes kamen).
/// - Was keinen Befund mehr ergeben kann, wird gar nicht geprüft: ein
///   Eintrag zu einem Seal mit gültigem Ref, der nicht früher ist als dieser.
/// - `trusted_key` sortiert ohne Prozess aus, was sicher nicht von einem
///   vertrauenswürdigen Schlüssel stammt (der Schlüssel, den die Signatur
///   nennt).
/// - Nur echte Prüfungen zählen gegen die Grenzen ([`MAX_CHECKS_PER_SEAL`]
///   je Seal, [`MAX_CHECKS_TOTAL`] insgesamt). Bleibt dabei ein Eintrag
///   ungeprüft, der noch einen Befund hätte ergeben können, steht
///   [`NoteCheck::exhausted`].
pub fn note_findings(
    entries: &[NoteEntry],
    seals: &BTreeSet<ContentHash>,
    project: &str,
    present: &dyn Fn(&ContentHash) -> bool,
    state: &dyn Fn(&ContentHash) -> FirstSightState,
    trusted_key: &dyn Fn(&str) -> bool,
    check: &CheckAnchor<'_>,
) -> NoteCheck {
    let mut out = NoteCheck::default();
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut parsed: Vec<(FirstSight, &NoteEntry)> = Vec::new();
    for entry in entries {
        if !seen.insert((entry.text.as_str(), entry.signature.as_str())) {
            continue;
        }
        let Ok(anchor) = FirstSight::parse(&entry.text) else {
            continue;
        };
        if anchor.project != project {
            out.other_project += 1;
            continue;
        }
        out.noted.insert(anchor.seal.clone());
        parsed.push((anchor, entry));
    }
    // Die früheste behauptete Sicht zuerst; was sich nicht lesen lässt, zum
    // Schluss (stabil, also sonst in Note-Reihenfolge).
    parsed.sort_by_key(|(anchor, _)| {
        anchor
            .at
            .parse::<jiff::Timestamp>()
            .map_or((1, None), |at| (0, Some(at)))
    });

    let mut checks: std::collections::BTreeMap<ContentHash, usize> = Default::default();
    let mut total = 0usize;
    // Seals mit einem gültig signierten Eintrag: dort ist entschieden.
    let mut settled: BTreeSet<ContentHash> = BTreeSet::new();
    let mut open = false;
    let mut best: std::collections::BTreeMap<ContentHash, NoteFinding> = Default::default();
    let mut states: std::collections::BTreeMap<ContentHash, FirstSightState> = Default::default();
    for (anchor, entry) in parsed {
        let in_session = seals.contains(&anchor.seal);
        if !in_session && !entry.merged {
            continue;
        }
        if in_session {
            out.mirrored += 1;
        }
        if settled.contains(&anchor.seal) {
            continue;
        }
        let seal_missing = !present(&anchor.seal);
        let current = if seal_missing {
            None
        } else {
            Some(
                states
                    .entry(anchor.seal.clone())
                    .or_insert_with(|| state(&anchor.seal))
                    .clone(),
            )
        };
        // Neben einem gültigen Ref kann nur eine **frühere** Sicht etwas
        // sagen (ein Hinweis) — alles andere kostet keine Prüfung.
        let stored_at = match &current {
            Some(FirstSightState::Anchored {
                anchor: stored,
                signature: AnchorSignature::Valid,
            }) => Some(stored.at.clone()),
            _ => None,
        };
        if let Some(stored_at) = &stored_at
            && !earlier(&anchor.at, stored_at)
        {
            continue;
        }
        if !trusted_key(&entry.signature) {
            out.forged += 1;
            out.unvalidated += 1;
            continue;
        }
        let checked = checks.entry(anchor.seal.clone()).or_default();
        if *checked >= MAX_CHECKS_PER_SEAL || total >= MAX_CHECKS_TOTAL {
            // Ungeprüft — zählt nur, wo noch ein Befund möglich war.
            open |= stored_at.is_none();
            continue;
        }
        let signed = check(&entry.text, &entry.signature);
        if signed.is_some() {
            *checked += 1;
            total += 1;
        }
        if signed == Some(false) {
            out.forged += 1;
            out.unvalidated += 1;
            continue;
        }
        match signed {
            Some(true) => {
                out.signed += 1;
                settled.insert(anchor.seal.clone());
            }
            _ => out.unvalidated += 1,
        }
        let seal = anchor.seal.clone();
        let pipeline = anchor.pipeline;
        let finding = match current {
            // Ohne Signer kein Hinweis: Den Seal kennt hier niemand, und
            // die Note kann jeder schreiben.
            None => signed.map(|_| NoteFinding::SealMissing { seal, pipeline }),
            Some(current) => match (signed, current) {
                (
                    Some(_),
                    FirstSightState::Anchored {
                        anchor: stored,
                        signature: AnchorSignature::Valid,
                    },
                ) => Some(NoteFinding::RefLater {
                    seal,
                    noted: pipeline,
                    stored: stored.pipeline,
                }),
                (_, current) if current.valid() => None,
                // `Some(false)` ist oben verworfen: hier heißt `Some` gültig.
                (Some(_), FirstSightState::Absent) => {
                    Some(NoteFinding::RefMissing { seal, pipeline })
                }
                (Some(_), _) => Some(NoteFinding::RefAltered { seal, pipeline }),
                // Ohne Signer kein Befund: Die Note kann jeder schreiben (der
                // Eintrag zählt als nicht belegt, `unvalidated`).
                (None, _) => None,
            },
        };
        if let Some(finding) = finding {
            let stronger = best
                .get(finding.seal())
                .is_none_or(|known| finding.rank() > known.rank());
            if stronger {
                best.insert(finding.seal().clone(), finding);
            }
        }
    }
    out.exhausted = open;
    out.findings = best.into_values().collect();
    out
}

/// Liegt `noted` zeitlich vor `stored`? Nicht vergleichbare Zeitpunkte
/// sind es nicht.
fn earlier(noted: &str, stored: &str) -> bool {
    match (
        noted.parse::<jiff::Timestamp>(),
        stored.parse::<jiff::Timestamp>(),
    ) {
        (Ok(noted), Ok(stored)) => noted < stored,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn anchor(byte: u8, project: &str) -> FirstSight {
        FirstSight {
            seal: seal(byte),
            project: project.into(),
            pipeline: 42,
            at: "2026-10-08T12:00:00Z".into(),
        }
    }

    const SIG: &str = "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n";
    const BAD: &str = "-----BEGIN SSH SIGNATURE-----\nBBBB\n-----END SSH SIGNATURE-----\n";

    /// Das Prädikat der Tests: `SIG` ist gültig, alles andere nicht.
    fn trusted(_: &str, signature: &str) -> Option<bool> {
        Some(signature == SIG)
    }

    fn untrusted(_: &str, _: &str) -> Option<bool> {
        None
    }

    fn present(text: Option<String>, signature: Option<&str>) -> FirstSightRef {
        FirstSightRef::Present {
            text,
            signature: signature.map(str::to_owned),
        }
    }

    #[test]
    fn only_a_parsed_signed_anchor_of_this_seal_counts() {
        let good = anchor(1, "g/r");
        let text = good.to_text().unwrap();
        let state = state_of(&seal(1), &present(Some(text.clone()), Some(SIG)), &trusted);
        assert!(state.valid());
        assert_eq!(
            state,
            FirstSightState::Anchored {
                anchor: good.clone(),
                signature: AnchorSignature::Valid
            }
        );
        // Unsigniert geprüft, ungeprüft, fremder Seal, kaputt, leer.
        let cases = [
            (
                state_of(&seal(1), &present(Some(text.clone()), Some(BAD)), &trusted),
                FirstSightState::Anchored {
                    anchor: good.clone(),
                    signature: AnchorSignature::Invalid,
                },
            ),
            (
                state_of(
                    &seal(1),
                    &present(Some(text.clone()), Some(SIG)),
                    &untrusted,
                ),
                FirstSightState::Anchored {
                    anchor: good.clone(),
                    signature: AnchorSignature::NotChecked,
                },
            ),
            (
                state_of(&seal(2), &present(Some(text.clone()), Some(SIG)), &trusted),
                FirstSightState::Unreadable,
            ),
            (
                state_of(
                    &seal(1),
                    &present(Some("junk\n".into()), Some(SIG)),
                    &trusted,
                ),
                FirstSightState::Unreadable,
            ),
            (
                state_of(&seal(1), &present(Some(text), None), &trusted),
                FirstSightState::Unreadable,
            ),
            (
                state_of(&seal(1), &FirstSightRef::Absent, &trusted),
                FirstSightState::Absent,
            ),
        ];
        for (state, expected) in cases {
            assert!(!state.valid());
            assert_eq!(state, expected);
        }
    }

    #[test]
    fn the_summary_holds_only_valid_anchors() {
        let valid = FirstSightState::Anchored {
            anchor: anchor(1, "g/r"),
            signature: AnchorSignature::Valid,
        };
        let unchecked = FirstSightState::Anchored {
            anchor: anchor(2, "g/r"),
            signature: AnchorSignature::NotChecked,
        };
        let states = [
            (seal(1), valid),
            (seal(2), unchecked),
            (seal(3), FirstSightState::Absent),
        ];
        let summary = summary(states.iter().map(|(s, st)| (s, st)));
        assert_eq!(summary.anchored, [seal(1)].into_iter().collect());
    }

    fn entry(byte: u8, project: &str, signature: &str) -> NoteEntry {
        NoteEntry {
            text: anchor(byte, project).to_text().unwrap(),
            signature: signature.into(),
            merged: true,
        }
    }

    /// Alle Seals liegen im Store.
    fn known(_: &ContentHash) -> bool {
        true
    }

    /// AC (EA-19): Ein gelöschter Ref neben einer gültig signierten Note
    /// ist ein Integritätsbefund — mit genau diesem Text.
    #[test]
    fn a_deleted_ref_with_a_signed_note_is_an_integrity_finding() {
        let seals: BTreeSet<ContentHash> = [seal(1), seal(2)].into_iter().collect();
        let entries = [entry(1, "g/r", SIG), entry(2, "g/r", SIG)];
        let state = |s: &ContentHash| {
            if *s == seal(2) {
                FirstSightState::Anchored {
                    anchor: anchor(2, "g/r"),
                    signature: AnchorSignature::Valid,
                }
            } else {
                FirstSightState::Absent
            }
        };
        let check = note_findings(&entries, &seals, "g/r", &known, &state, &any_key, &trusted);
        assert_eq!(check.mirrored, 2);
        assert_eq!(
            check.findings,
            vec![NoteFinding::RefMissing {
                seal: seal(1),
                pipeline: 42
            }]
        );
        assert!(check.findings[0].integrity());
        assert_eq!(
            check.findings[0].text(),
            format!(
                "seal {}: anchor ref missing, MR note present (pipeline #42)",
                seal(1)
            )
        );
    }

    /// Eine Note kann jeder schreiben: Ein Eintrag mit falscher Signatur,
    /// aus einem anderen Projekt oder für einen fremden, vorhandenen Seal
    /// ist nie ein Befund; ohne Signer bleibt es ein Hinweis.
    #[test]
    fn unsigned_or_foreign_note_entries_never_make_a_finding() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let run = |entries: &[NoteEntry], check: &CheckAnchor<'_>| {
            note_findings(entries, &seals, "g/r", &known, &absent, &any_key, check)
        };
        let forged = run(&[entry(1, "g/r", BAD)], &trusted);
        assert_eq!(forged.findings, Vec::new());
        assert_eq!(forged.forged, 1);
        let other = run(&[entry(1, "fork/r", SIG)], &trusted);
        assert_eq!(
            other,
            NoteCheck {
                other_project: 1,
                ..NoteCheck::default()
            }
        );
        // Ein fremder Seal aus einem nicht gemergten Merge Request: nie.
        let unmerged = NoteEntry {
            merged: false,
            ..entry(9, "g/r", SIG)
        };
        let foreign = run(&[unmerged], &trusted);
        assert_eq!(
            foreign,
            NoteCheck {
                noted: [seal(9)].into_iter().collect(),
                ..NoteCheck::default()
            }
        );
        // Aus einem gemergten zählt auch ein fremder Seal — dorthin spiegelt
        // nur `minds anchor --mirror` (Security-Review EA-19).
        let merged = run(&[entry(9, "g/r", SIG)], &trusted);
        assert_eq!(
            merged.findings,
            vec![NoteFinding::RefMissing {
                seal: seal(9),
                pipeline: 42
            }]
        );
        let unchecked = run(&[entry(1, "g/r", SIG)], &untrusted);
        assert_eq!(unchecked.findings, Vec::new());
        assert_eq!(unchecked.unvalidated, 1);
    }

    /// Ein ersetzter Ref (Müll oder fremde Signatur) neben einer gültigen
    /// Note ist ebenso ein Befund; zwei Notes zum selben Seal ergeben einen
    /// Befund, nicht zwei. Ein gültiger, aber späterer Ref ist ein Hinweis;
    /// ein gültiger, gleich alter Ref erledigt alles.
    #[test]
    fn a_replaced_ref_is_altered_and_findings_are_per_seal() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let junk = |_: &ContentHash| FirstSightState::Unreadable;
        let entries = [entry(1, "g/r", SIG), entry(1, "g/r", SIG)];
        let check = note_findings(&entries, &seals, "g/r", &known, &junk, &any_key, &trusted);
        assert_eq!(
            check.findings,
            vec![NoteFinding::RefAltered {
                seal: seal(1),
                pipeline: 42
            }]
        );
        let later = |_: &ContentHash| FirstSightState::Anchored {
            anchor: FirstSight {
                pipeline: 43,
                at: "2026-10-09T12:00:00Z".into(),
                ..anchor(1, "g/r")
            },
            signature: AnchorSignature::Valid,
        };
        let check = note_findings(&entries, &seals, "g/r", &known, &later, &any_key, &trusted);
        assert_eq!(
            check.findings,
            vec![NoteFinding::RefLater {
                seal: seal(1),
                noted: 42,
                stored: 43
            }]
        );
        assert!(check.findings[0].integrity());
        let same = |_: &ContentHash| FirstSightState::Anchored {
            anchor: anchor(1, "g/r"),
            signature: AnchorSignature::Valid,
        };
        assert_eq!(
            note_findings(&entries, &seals, "g/r", &known, &same, &any_key, &trusted).findings,
            Vec::new()
        );
    }

    /// Wer Seal und Anker zusammen löscht, fällt an der Note genauso auf —
    /// aber nur mit gültig signiertem Eintrag.
    #[test]
    fn a_deleted_seal_with_a_signed_note_is_an_integrity_finding() {
        let seals = BTreeSet::new();
        let gone = |s: &ContentHash| *s != seal(7);
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let check = note_findings(
            &[entry(7, "g/r", SIG)],
            &seals,
            "g/r",
            &gone,
            &absent,
            &any_key,
            &trusted,
        );
        assert_eq!(
            check.findings,
            vec![NoteFinding::SealMissing {
                seal: seal(7),
                pipeline: 42
            }]
        );
        assert!(check.findings[0].integrity());
        let bad = note_findings(
            &[entry(7, "g/r", BAD)],
            &seals,
            "g/r",
            &gone,
            &absent,
            &any_key,
            &trusted,
        );
        assert_eq!(bad.findings, Vec::new());
        let unchecked = note_findings(
            &[entry(7, "g/r", SIG)],
            &seals,
            "g/r",
            &gone,
            &absent,
            &any_key,
            &untrusted,
        );
        assert_eq!(unchecked.findings, Vec::new());
    }

    /// Jeder Schlüssel könnte vertrauenswürdig sein (keine Vorauswahl).
    fn any_key(_: &str) -> bool {
        true
    }

    /// Eine armierte Signatur, die sich von allen anderen unterscheidet.
    fn bad(n: usize) -> String {
        format!("-----BEGIN SSH SIGNATURE-----\nBAD{n}\n-----END SSH SIGNATURE-----\n")
    }

    /// Je Seal startet höchstens [`MAX_CHECKS_PER_SEAL`]-mal `ssh-keygen`,
    /// gleich wie viele Einträge eine Note trägt — gleiche Einträge zählen
    /// einmal.
    #[test]
    fn signature_checks_per_seal_are_capped() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let calls = std::cell::Cell::new(0);
        let counting = |_: &str, _: &str| {
            calls.set(calls.get() + 1);
            Some(false)
        };
        let flood: Vec<NoteEntry> = (0..100).map(|n| entry(1, "g/r", &bad(n))).collect();
        let check = note_findings(&flood, &seals, "g/r", &known, &absent, &any_key, &counting);
        assert_eq!(calls.get(), MAX_CHECKS_PER_SEAL);
        assert_eq!(check.mirrored, 100);
        assert!(check.exhausted);
        calls.set(0);
        let same: Vec<NoteEntry> = (0..100).map(|_| entry(1, "g/r", BAD)).collect();
        let check = note_findings(&same, &seals, "g/r", &known, &absent, &any_key, &counting);
        assert_eq!(calls.get(), 1);
        assert!(!check.exhausted);
    }

    /// Security-Review EA-19: Vier gefälschte Einträge vor dem echten
    /// verdrängen ihn nie still — ohne Vorauswahl ist der Abgleich dann
    /// unvollständig, mit Vorauswahl (Schlüssel keines Signers) kosten die
    /// Fälschungen nichts, und der gelöschte Ref fällt auf.
    #[test]
    fn forged_entries_cannot_push_the_real_one_out() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let mut entries: Vec<NoteEntry> = (0..4).map(|n| entry(1, "g/r", &bad(n))).collect();
        entries.push(entry(1, "g/r", SIG));

        let check = note_findings(&entries, &seals, "g/r", &known, &absent, &any_key, &trusted);
        assert!(check.exhausted);
        assert_eq!(check.findings, Vec::new());

        let only_sig = |signature: &str| signature == SIG;
        let check = note_findings(
            &entries, &seals, "g/r", &known, &absent, &only_sig, &trusted,
        );
        assert!(!check.exhausted);
        assert_eq!(check.forged, 4);
        assert_eq!(check.signed, 1);
        assert_eq!(
            check.findings,
            vec![NoteFinding::RefMissing {
                seal: seal(1),
                pipeline: 42
            }]
        );
    }

    /// Auch über viele Seals hinweg ist die Zahl der Prüfungen begrenzt.
    #[test]
    fn signature_checks_are_capped_in_total() {
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let missing = |_: &ContentHash| false;
        let calls = std::cell::Cell::new(0);
        let counting = |_: &str, _: &str| {
            calls.set(calls.get() + 1);
            Some(false)
        };
        let flood: Vec<NoteEntry> = (0..=255u8)
            .chain(0..=255u8)
            .enumerate()
            .map(|(n, byte)| entry(byte, "g/r", &bad(n)))
            .collect();
        let check = note_findings(
            &flood,
            &BTreeSet::new(),
            "g/r",
            &missing,
            &absent,
            &any_key,
            &counting,
        );
        assert_eq!(calls.get(), MAX_CHECKS_TOTAL);
        assert!(check.exhausted);
    }

    /// Ohne Signer startet keine Prüfung einen Prozess — dann erschöpft
    /// auch keine Flut die Grenzen.
    #[test]
    fn unchecked_entries_never_exhaust_the_caps() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let flood: Vec<NoteEntry> = (0..10).map(|n| entry(1, "g/r", &bad(n))).collect();
        let check = note_findings(&flood, &seals, "g/r", &known, &absent, &any_key, &untrusted);
        assert!(!check.exhausted);
        assert_eq!(check.findings, Vec::new());
        assert_eq!(check.unvalidated, 10);
    }

    /// Einen offenen Merge Request kann jeder anlegen und echte Einträge
    /// hineinkopieren: Ein fehlender Seal zählt nur aus gemergten.
    #[test]
    fn a_missing_seal_counts_only_from_merged_requests() {
        let gone = |_: &ContentHash| false;
        let absent = |_: &ContentHash| FirstSightState::Absent;
        let copied = NoteEntry {
            merged: false,
            ..entry(7, "g/r", SIG)
        };
        let check = note_findings(
            &[copied],
            &BTreeSet::new(),
            "g/r",
            &gone,
            &absent,
            &any_key,
            &trusted,
        );
        assert_eq!(
            check,
            NoteCheck {
                noted: [seal(7)].into_iter().collect(),
                ..NoteCheck::default()
            }
        );
    }

    /// Neben einem gültigen Ref kostet ein Eintrag, der nicht früher ist,
    /// keine Prüfung — eine Flut solcher Einträge erschöpft nichts.
    #[test]
    fn entries_that_cannot_matter_are_not_checked() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let valid = |_: &ContentHash| FirstSightState::Anchored {
            anchor: anchor(1, "g/r"),
            signature: AnchorSignature::Valid,
        };
        let calls = std::cell::Cell::new(0);
        let counting = |_: &str, _: &str| {
            calls.set(calls.get() + 1);
            Some(false)
        };
        let flood: Vec<NoteEntry> = (0..20).map(|n| entry(1, "g/r", &bad(n))).collect();
        let check = note_findings(&flood, &seals, "g/r", &known, &valid, &any_key, &counting);
        assert_eq!(calls.get(), 0);
        assert!(!check.exhausted);
        assert_eq!(check.findings, Vec::new());
    }

    /// Die früheste Sicht zuerst: Steht die spätere Note vorn, geht der
    /// Hinweis auf die frühere trotzdem nicht verloren.
    #[test]
    fn the_earliest_sight_is_checked_first() {
        let seals: BTreeSet<ContentHash> = [seal(1)].into_iter().collect();
        let later = FirstSight {
            pipeline: 43,
            at: "2026-10-09T12:00:00Z".into(),
            ..anchor(1, "g/r")
        };
        let stored = later.clone();
        let state = move |_: &ContentHash| FirstSightState::Anchored {
            anchor: stored.clone(),
            signature: AnchorSignature::Valid,
        };
        let entries = [
            NoteEntry {
                text: later.to_text().unwrap(),
                signature: SIG.into(),
                merged: true,
            },
            entry(1, "g/r", SIG),
        ];
        let check = note_findings(&entries, &seals, "g/r", &known, &state, &any_key, &trusted);
        assert_eq!(
            check.findings,
            vec![NoteFinding::RefLater {
                seal: seal(1),
                noted: 42,
                stored: 43
            }]
        );
    }
}
