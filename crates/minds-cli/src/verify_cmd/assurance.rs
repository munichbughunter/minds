//! Die Assurance- und `Not proven`-Zeile von `minds verify`, das Gate
//! `--require-assurance` und der Abgleich mit dem Ledger des Witness
//! (EA-12).
//!
//! Die Stufe rechnet [`minds_reader::assurance::assess`] — rein, aus Fakten
//! zur Lesezeit (W2/W5). Hier werden diese Fakten aus dem Store und der
//! Witness-Prüfung zusammengetragen und das Ergebnis gesetzt. Nichts davon
//! wird gespeichert.
//!
//! # Was hier (noch) nicht gelesen wird
//!
//! - **Witness-Starts** (Profil, Trennungsbefund): seit den Witness-Fakten hält der
//!   Witness sie beim Versiegeln im Observation-Objekt jeder Epoche fest
//!   ([`minds_reader::observations::witness_facts`]); gelesen werden genau
//!   die Epochen, die das Fenster der Session tragen. Eine Epoche ohne
//!   Fakten (älteres Schema, fehlendes Objekt) zählt als Start ohne Profil
//!   (`witness profile unknown`), fail-closed.
//! - **Gegenzeichnungen** (EA-19) kommen aus
//!   `refs/minds/anchors/first-sight/` ([`WitnessTrust::first_sight`]);
//!   zählen kann eine nur unter `minds-anchor` gegen dieselbe Signer-Datei.
//!   Der **Intent** (EA-14/EA-15) wird gelesen
//!   ([`minds_reader::intent::intent_of`]); seine Signatur gilt nur unter
//!   `minds-intent` gegen dieselbe vertrauenswürdige Signer-Datei. Der
//!   **Replay** (EA-18b) kommt aus den Records unter
//!   `refs/minds/anchors/replay/` ([`WitnessTrust::replay`]); zählen kann er
//!   nur unter `minds-anchor` gegen dieselbe Signer-Datei — ein
//!   unsignierter Record steht als `replay record unsigned` da.
//!
//! # Das Ledger (`--witness-home`)
//!
//! Das Ledger ist die append-only Liste der Seals, die der Witness erzeugt
//! hat (`<seal-id> <scope> <last_event_at>` je Zeile). `verify` liest es nur,
//! wenn es ausdrücklich genannt wird, und nur lesend — geschrieben wird es
//! allein vom Witness (W1). Fehlt ein dort genannter Seal unter
//! `refs/minds/evidence/`, ist das Repository verändert worden: ein
//! Integritätsbefund (`TAMPERED`, Exit 1), keine Assurance-Frage.

use std::collections::BTreeSet;
use std::path::Path;

use minds_core::evidence::{SCOPE_WITNESS_FS_V1, SCOPE_WITNESS_V1, Seal, SealOutcome, limits_at};
use minds_core::{ContentHash, EvidenceSource, Session, SessionId};
use minds_reader::assurance::{
    AssuranceInput, AssuranceLevel, AssuranceReport, FsCoverage, IntentState, LedgerCheck,
    MAX_LISTED, RangeInput, Reason, SealMaterial, Seals, SessionEnd, Signers, assess,
};
use minds_reader::model::EvidenceVerdict;
use minds_reader::observations::{Window, WindowObservations};
use minds_store::{ContextStore, StoreError};

use super::witness_trust::WitnessTrust;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Die Breite des Stufen-Worts (`A2 witnessed   (…)`).
const LEVEL_WIDTH: usize = 15;

/// So viele Grenzen nennt die Kurzzeile; der Rest steht hinter `--limits`.
const NOT_PROVEN_SHORT: usize = 3;

/// Höchstgröße des Ledgers, die gelesen wird (64 MiB — eine Zeile je Seal,
/// rund 120 Bytes: eine halbe Million Seals).
const MAX_LEDGER_BYTES: u64 = 64 * 1024 * 1024;

/// Liest `--require-assurance`: genau `A0` … `A3`.
pub(crate) fn parse_required(raw: &str) -> Result<AssuranceLevel, String> {
    AssuranceLevel::parse(raw).ok_or_else(|| {
        format!(
            "--require-assurance expects A0, A1, A2 or A3, got \"{}\"",
            crate::text::sanitize(raw)
        )
    })
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

/// Gleicht das Ledger in `home` mit den Seals des Repositorys ab. Liefert
/// den Abgleich und ob das Ledger in einer abgerissenen Zeile endet.
///
/// Ein nicht lesbares Ledger ist ein operativer Fehler (Exit 4), nie
/// „abgeglichen": Ein Gate, das still wegfällt, wäre schlimmer als ein
/// lauter Abbruch. Eine abgerissene **letzte** Zeile (Absturz des Witness
/// mitten im Schreiben, ohne Zeilenende) zählt nicht als Eintrag — wird aber
/// gemeldet: Der Witness hängt danach nichts mehr an (`incomplete witness
/// ledger`), spätere Seals schützt das Ledger also nicht. Jede andere Zeile
/// muss die Form des Witness haben (`<seal-id> <scope> <last_event_at>`),
/// sonst ist es ein Fehler.
pub(crate) fn check_ledger(home: &Path, store: &dyn ContextStore) -> Fallible<Ledger> {
    use std::io::Read;
    let path = home.join("ledger");
    let unreadable =
        |err: std::io::Error| format!("witness ledger {} unreadable: {err}", path.display());
    let file = std::fs::File::open(&path).map_err(unreadable)?;
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(format!("witness ledger {} is not a file", path.display()).into());
    }
    // Eine Lesung, begrenzt: keine Lücke zwischen Größenprüfung und Lesen.
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err(format!(
            "witness ledger {} exceeds {MAX_LEDGER_BYTES} bytes",
            path.display()
        )
        .into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("witness ledger {} is not UTF-8", path.display()))?;
    let (ids, torn) = ledger_ids(&text)?;
    let mut missing = Vec::new();
    let mut altered = Vec::new();
    for id in ids {
        // Mit Hash-Prüfung: Ein Ref, unter dem andere Bytes liegen, ist so
        // wenig der bezeugte Seal wie ein fehlender — wer ihn ersetzt statt
        // löscht, darf nicht durchrutschen.
        match store.seal_text(&id) {
            Ok(Some(text)) if Seal::parse(&text).is_ok() => {}
            Ok(Some(_)) | Err(StoreError::SealMismatch { .. }) => altered.push(id),
            Ok(None) => missing.push(id),
            // Liegen unter dem Ref Bytes, die nicht einmal Text sind, ist
            // der bezeugte Seal ebenso ersetzt — TAMPERED, nicht „operativ".
            Err(err) => match store.seal_bytes(&id) {
                Ok(Some(_)) => altered.push(id),
                _ => return Err(err.into()),
            },
        }
    }
    let check = if missing.is_empty() && altered.is_empty() {
        LedgerCheck::Complete
    } else {
        LedgerCheck::Missing(missing.iter().chain(&altered).cloned().collect())
    };
    Ok(Ledger {
        check,
        missing,
        altered,
        torn,
    })
}

/// Der Abgleich mit dem Ledger, wie `verify` ihn ausspricht.
pub(crate) struct Ledger {
    /// Für [`assess`]: fehlende **und** veränderte Seals — beides heißt, das
    /// bezeugte Material liegt nicht mehr im Repository.
    pub check: LedgerCheck,
    /// Bezeugte Seals ohne Ref.
    pub missing: Vec<ContentHash>,
    /// Bezeugte Seals, deren Ref auf andere Bytes zeigt.
    pub altered: Vec<ContentHash>,
    /// Das Ledger endet in einer abgerissenen Zeile.
    pub torn: bool,
}

impl Ledger {
    /// Ohne `--witness-home`.
    pub(crate) fn not_checked() -> Self {
        Self {
            check: LedgerCheck::NotChecked,
            missing: Vec::new(),
            altered: Vec::new(),
            torn: false,
        }
    }
}

/// Die Hinweiszeile zu einem abgerissenen Ledger-Ende.
pub(super) const TORN_LEDGER_NOTE: &str =
    "Note           witness ledger ends in a torn line — seals after it are not ledgered";

/// Die Seal-Ids des Ledgers, sortiert und ohne Dubletten, und ob die letzte
/// Zeile abgerissen ist.
fn ledger_ids(text: &str) -> Fallible<(BTreeSet<ContentHash>, bool)> {
    let mut ids = BTreeSet::new();
    let mut torn = false;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if index + 1 == lines.len() && !line.ends_with('\n') {
            torn = true;
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let id = match fields[..] {
            [id, _scope, _at] => id.parse::<ContentHash>().ok(),
            _ => None,
        };
        let Some(id) = id else {
            return Err(format!("witness ledger line {} is unreadable", index + 1).into());
        };
        ids.insert(id);
    }
    Ok((ids, torn))
}

/// Die Zeilen des Ledger-Befunds für die Integritäts-Achse — leer, wenn
/// nichts fehlt. Die erste ergänzt die `Integrity`-Zeile, weitere stehen
/// eingerückt darunter.
pub(super) fn ledger_findings(ledger: &Ledger) -> Vec<String> {
    let all: Vec<String> = ledger
        .missing
        .iter()
        .map(|id| format!("witnessed seal {id} missing from the repository"))
        .chain(
            ledger
                .altered
                .iter()
                .map(|id| format!("witnessed seal {id} altered in the repository")),
        )
        .collect();
    let mut lines: Vec<String> = all.iter().take(MAX_LISTED).cloned().collect();
    if all.len() > MAX_LISTED {
        lines.push(format!("… {} more", all.len() - MAX_LISTED));
    }
    lines
}

// ---------------------------------------------------------------------------
// Fakten einer Session
// ---------------------------------------------------------------------------

/// Was der Verdikt-Pfad über eine Session schon weiß — dieselbe Rechnung
/// wie die Integritäts- und Coverage-Achse, damit Stufe und Verdikt nie
/// über verschiedenes Material urteilen.
pub(crate) struct Facts<'a> {
    /// Die geprüfte Session.
    pub id: SessionId,
    /// Die Session, gelesen mit Hash-Prüfung; `None`, wenn nicht lesbar.
    pub session: Option<&'a Session>,
    /// Die lesbaren Seals der Session (Id, geparster Seal, gespeicherter
    /// Text).
    pub seals: &'a [(ContentHash, Seal, String)],
    /// Kein Seal-Material: vor der Evidence-Chain erfasst.
    pub legacy: bool,
    /// Die Integritäts-Achse sagt `VIOLATED`.
    pub tampered: bool,
    /// Die Coverage-Achse ist vollständig.
    pub complete: bool,
    /// Die Epochen-Kette schließt sich, und jeder genannte Seal ist lesbar.
    pub chain_closed: bool,
    /// Woher die Verknüpfung mit dem geprüften Commit stammt.
    pub link: Option<EvidenceSource>,
    /// Der Ledger-Abgleich.
    pub ledger: &'a LedgerCheck,
    /// Die Abdeckung durch den Datei-Beobachter.
    pub observations: &'a FsCoverage,
    /// Die Witness-Starts aus den Epochen der Fenster
    /// ([`minds_reader::observations::witness_starts`]).
    pub witness_starts: &'a [minds_reader::assurance::WitnessStart],
    /// Die Intent-Lage ([`intent_state`]) — einmal je Session gerechnet,
    /// damit Coverage (Scope-Befunde, EA-17) und Stufe denselben Anker
    /// sehen.
    pub intent: &'a IntentState,
    /// Der geprüfte Commit (volle Hex-Id) — nur Replay-Records genau dieses
    /// Commits zählen (EA-18b); `None` ohne Commit-Kontext.
    pub commit: Option<&'a str>,
}

/// Rechnet die Assurance der Session aus.
pub(crate) fn report(trust: &WitnessTrust<'_>, facts: &Facts<'_>) -> AssuranceReport {
    let mut ranges: Vec<RangeInput> = facts
        .seals
        .iter()
        .map(|(seal_id, seal, text)| {
            let witness = matches!(seal.scope.as_str(), SCOPE_WITNESS_V1 | SCOPE_WITNESS_FS_V1);
            RangeInput {
                seal: seal_id.clone(),
                scope: seal.scope.clone(),
                // Nur Witness-Scopes tragen eine Witness-Signatur; für die
                // übrigen startet keine Prüfung `ssh-keygen`.
                signature: if witness {
                    trust.signature(seal_id, text)
                } else {
                    minds_reader::assurance::SealSignature::NotChecked
                },
                quiescence: quiescence(facts, seal),
                closes_session: closes_session(facts, seal),
            }
        })
        .collect();
    // Zeitliche Reihenfolge: Der letzte Bereich ist das Ende der Session.
    let order: Vec<(Option<jiff::Timestamp>, u64)> = facts
        .seals
        .iter()
        .map(|(_, seal, _)| range_order(seal))
        .collect();
    let mut indexed: Vec<(usize, RangeInput)> = ranges.drain(..).enumerate().collect();
    indexed.sort_by_key(|(index, _)| order[*index]);
    let ranges: Vec<RangeInput> = indexed.into_iter().map(|(_, range)| range).collect();

    let replay = trust.replay(facts.id, facts.session, facts.commit);
    // Gegenzeichnungen (EA-19): nur gültig signierte zählen — gezählt wird
    // ohnehin erst in `assess`, und nur über geprüftem, intaktem Material.
    let states: Vec<(ContentHash, minds_reader::first_sight::FirstSightState)> = facts
        .seals
        .iter()
        .map(|(seal_id, _, _)| (seal_id.clone(), trust.first_sight(seal_id)))
        .collect();
    let anchors = minds_reader::first_sight::summary(states.iter().map(|(id, state)| (id, state)));
    let integrity = if facts.tampered {
        EvidenceVerdict::Tampered
    } else if facts.complete {
        EvidenceVerdict::Verified
    } else {
        EvidenceVerdict::Incomplete
    };
    let seal = SealMaterial {
        seals: if facts.legacy {
            Seals::Legacy
        } else {
            Seals::Sealed(&ranges)
        },
        integrity,
        chain_closed: facts.chain_closed,
        ledger: facts.ledger,
        link: facts.link,
        intent: facts.intent,
    };
    let signers = Signers {
        witness_starts: facts.witness_starts,
        observations: facts.observations,
        replay: replay.as_ref(),
        anchors: Some(&anchors),
    };
    // Dieselbe Tür wie der Index (`minds_reader::assurance::assurance`):
    // ohne vertrauenswürdige Signer zählt nichts, was nur mit ihnen zählte
    // — und der Report trägt die Stufe (`rating`), die jede Oberfläche zeigt.
    assess(&AssuranceInput::of(
        &seal,
        trust.trusted().then_some(&signers),
    ))
}

/// Die Intent-Lage der Session (EA-14/EA-15), zur Lesezeit aus dem Store:
/// die Epochen-Kette nur über gültig signierte Witness-Seals
/// ([`WitnessTrust::witnessed`]), die Signatur des Ankers unter
/// `minds-intent` gegen dieselbe vertrauenswürdige Signer-Datei. Ob davon
/// etwas „bezeugt" heißen darf, entscheidet danach `assess` (ohne intaktes,
/// geprüftes Material: nichts).
pub(crate) fn intent_state(
    trust: &WitnessTrust<'_>,
    id: SessionId,
    session: Option<&Session>,
    seals: &[(ContentHash, Seal, String)],
) -> IntentState {
    let Some(session) = session else {
        return IntentState::Unbound;
    };
    let seals: Vec<(ContentHash, Seal)> = seals
        .iter()
        .map(|(id, seal, _)| (id.clone(), seal.clone()))
        .collect();
    // Gelten alle Witness-Bereiche der Session unter einem getrennten
    // Principal (A2 steht in Frage), zählt auch in der Kette davor nur ein
    // solcher — ein Vorgänger unter einem Schlüssel, den der Agent lesen
    // kann, belegte sonst den Anfang des Intents.
    let separated = |seal_id: &ContentHash| {
        trust
            .store()
            .seal_text(seal_id)
            .ok()
            .flatten()
            .is_some_and(|text| trust.signature(seal_id, &text).is_separated_witness())
    };
    let mut witness_ranges = seals
        .iter()
        .filter(|(_, seal)| seal.scope == SCOPE_WITNESS_V1)
        .peekable();
    let all_separated =
        witness_ranges.peek().is_some() && witness_ranges.all(|(seal_id, _)| separated(seal_id));
    let witnessed = |seal_id: &ContentHash, _: &Seal| {
        if all_separated {
            separated(seal_id)
        } else {
            trust.witnessed(seal_id)
        }
    };
    let chain =
        minds_reader::intent::epoch_chain(trust.store(), id, session.clone(), &seals, &witnessed);
    let check = |anchor: &str, signature: &str| trust.intent_signature(anchor, signature);
    minds_reader::intent::intent_of(&chain, trust.store(), &check)
}

/// Die zeitliche Ordnung der Bereiche — dieselbe für die `Seal`-Zeilen des
/// Blocks und für `range N` in der Assurance-Zeile, damit die Nummer auf die
/// N-te gedruckte Zeile zeigt.
pub(super) fn range_order(seal: &Seal) -> (Option<jiff::Timestamp>, u64) {
    (seal.last_event_at.parse().ok(), seal.first_seq)
}

/// Die Herkunft der Session, die der Seal nennt: nur, wenn er genau die
/// geprüfte Session nennt — deren Objekt hat der Verdikt-Pfad mit
/// Hash-Prüfung gelesen. Alles andere (Block-Seal, eine andere Session,
/// nicht lesbar): `None`, fail-closed.
fn sealed_lineage<'f>(facts: &Facts<'f>, seal: &Seal) -> Option<&'f minds_core::Lineage> {
    let SealOutcome::Stored { session } = &seal.outcome else {
        return None;
    };
    if session.parse::<SessionId>().ok() != Some(facts.id) {
        return None;
    }
    facts.session.and_then(|session| session.lineage.as_ref())
}

/// Ob der Bereich mit dem Ende der Session schließt (`lineage.closed`,
/// [`sealed_lineage`]); sonst `false`, fail-closed.
fn closes_session(facts: &Facts<'_>, seal: &Seal) -> bool {
    sealed_lineage(facts, seal).is_some_and(|lineage| lineage.closed)
}

/// Ob die Session am Ende des Bereichs ruhte (`lineage.quiescence`,
/// [`sealed_lineage`]); sonst `None`, fail-closed.
fn quiescence(facts: &Facts<'_>, seal: &Seal) -> Option<minds_core::Quiescence> {
    sealed_lineage(facts, seal).and_then(|lineage| lineage.quiescence)
}

/// Die Abdeckung durch den Datei-Beobachter: lückenlos, wenn der Witness
/// ein Fenster über die Session trägt, keine seiner Epochen ein Glied der
/// Kette verlor und der Beobachter in keiner blind war; ohne Fenster nicht
/// verfügbar.
///
/// Die Gründe sind die der `fs.gap`-Events, wie das Observation-Objekt der
/// Epoche sie trägt (`overflow`, `panic`, …). Ein verlorenes Kettenglied
/// (`gaps=` im Seal) hat keinen Grund im Material: `observation gap` ohne
/// Klammer.
pub(crate) fn fs_coverage(store: &dyn ContextStore, windows: &[Window]) -> FsCoverage {
    if windows.is_empty() {
        return FsCoverage::Unavailable { cause: None };
    }
    // Ein Fenster ohne benannte Epochen („jede vertrauenswürdige") belegt
    // keine lückenlose Kette — fail-closed.
    if windows.iter().any(|window| window.epochs.is_none()) {
        return FsCoverage::Unavailable { cause: None };
    }
    let mut gapped = false;
    let mut reasons: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for epoch in windows
        .iter()
        .flat_map(|window| window.epochs.iter().flatten())
    {
        // Unlesbar heißt: nicht belegt lückenlos.
        let Some(seal) = store
            .seal_text(epoch)
            .ok()
            .flatten()
            .and_then(|text| Seal::parse(&text).ok())
        else {
            gapped = true;
            continue;
        };
        if seal.gaps > 0 {
            gapped = true;
        }
        let SealOutcome::ObservationsStored { observations } = &seal.outcome else {
            continue;
        };
        let object = observations
            .parse::<ContentHash>()
            .ok()
            .and_then(|id| store.get_observations(&id).ok().flatten());
        if let Some(object) = object {
            for gap in &object.gaps {
                gapped = true;
                reasons.insert(gap.reason.word().to_owned());
            }
        }
    }
    if gapped {
        FsCoverage::Gapped {
            reasons: reasons.into_iter().collect(),
        }
    } else {
        FsCoverage::Complete
    }
}

/// Die Witness-Starts der Fenster: die Fakten ihrer Epochen
/// ([`minds_reader::observations::witness_facts`]), „getrennt" nur, wenn der
/// Seal der Epoche unter einem Principal gilt, den ein Mensch als getrennten
/// Witness eingetragen hat
/// ([`minds_core::evidence::SEPARATED_WITNESS_PRINCIPAL`]).
pub(crate) fn witness_starts(
    trust: &WitnessTrust<'_>,
    store: &dyn ContextStore,
    windows: &[Window],
) -> Vec<minds_reader::assurance::WitnessStart> {
    let separated = |seal_id: &ContentHash| {
        store
            .seal_text(seal_id)
            .ok()
            .flatten()
            .is_some_and(|text| trust.signature(seal_id, &text).is_separated_witness())
    };
    let mut starts = minds_reader::observations::witness_starts(
        &minds_reader::observations::witness_facts(store, windows),
        &separated,
    );
    // Ein Fenster ohne benannte Epochen trägt keine Fakten — für seinen
    // Zeitraum ist das Profil unbekannt (fail-closed).
    if windows.iter().any(|window| window.epochs.is_none()) {
        starts.push(minds_reader::assurance::WitnessStart {
            profile: None,
            isolated: false,
            key: None,
        });
    }
    starts
}

/// Die Schreib-Claims der Session, die der Witness nicht bestätigt — als
/// Detailzeilen unter der Coverage-Zeile, gekappt auf
/// [`super::artifact::DETAIL_CAP`] (außer mit `all`).
///
/// Nur mit Beobachtungsfenster: Ohne bezeugtes Fenster hat niemand
/// beobachtet, und „unbestätigt" wäre über jeden Claim wahr und damit
/// nichtssagend. Die Stelle ist Zug und Aufruf in der gespeicherten Session
/// (1-basiert) — die Event-`seq` des Journals steht nicht im Store. War
/// der Beobachter im Fenster des Claims blind ([`gap_near`]), sagt die
/// Zeile das (`observation gap (overflow: 12 events dropped)`) statt
/// `no file-system observation`.
pub(super) fn uncorroborated_lines(
    root: &Path,
    id: SessionId,
    session: &Session,
    observations: &WindowObservations,
    all: bool,
) -> Vec<String> {
    use minds_reader::observations::{Corroboration, corroborations, gap_near};
    let spellings = minds_reader::artifact::root_spellings(root);
    let roots: Vec<&Path> = spellings.iter().map(std::path::PathBuf::as_path).collect();
    let claims: Vec<String> = corroborations(&[session], &roots, &observations.observations)
        .into_iter()
        .filter(|claim| claim.corroboration == Corroboration::Uncorroborated)
        .filter_map(|claim| {
            let turn = session.turns.get(claim.turn)?;
            let call = turn.tool_calls.get(claim.call)?;
            let effect = call.effect.as_ref()?;
            let written = effect.written.as_ref()?.to_string();
            let path = super::artifact::shown_path(claim.path.as_deref()?);
            // Dieselbe Claim-Zeit wie die Korroboration: die Meldung, sonst
            // der Zug.
            let claimed_at = effect
                .written_at
                .as_deref()
                .or(turn.at.as_deref())
                .and_then(|at| at.parse::<jiff::Timestamp>().ok());
            let why = match claimed_at.and_then(|at| gap_near(&observations.gaps, at)) {
                Some(gap) => format!("observation gap ({})", gap.describe()),
                None => "no file-system observation".to_owned(),
            };
            Some(format!(
                "  uncorroborated  turn {} call {}  {} {path}  {}…  {why}",
                claim.turn + 1,
                claim.call + 1,
                shown_tool(&call.name),
                written.get(..11).unwrap_or(&written),
            ))
        })
        .collect();
    let cap = if all {
        claims.len()
    } else {
        super::artifact::DETAIL_CAP
    };
    let mut lines: Vec<String> = claims.iter().take(cap).cloned().collect();
    if claims.len() > lines.len() {
        lines.push(format!(
            "  … {} more (minds verify {id} --all)",
            claims.len() - lines.len()
        ));
    }
    lines
}

/// Höchstens so viele Zeichen eines Tool-Namens — er stammt vom Agenten.
const TOOL_CAP: usize = 40;

/// Ein Tool-Name zur Anzeige: gekürzt, entschärft und ohne Leerraum — ein
/// Name wie `Write x  b3-…  corroborated` täuschte sonst Spalten vor.
fn shown_tool(name: &str) -> String {
    let shown = match name.char_indices().nth(TOOL_CAP) {
        Some((cut, _)) => format!("{}…", crate::text::sanitize(&name[..cut])),
        None => crate::text::sanitize(name),
    };
    shown
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

// ---------------------------------------------------------------------------
// Ausgabe
// ---------------------------------------------------------------------------

/// Die `Assurance`-Zeile — Wort und Klammer aus der einen Stufe des Reports
/// ([`AssuranceReport::rating`]), die auch die TUI zeigt. Unter A2 steht in
/// der Klammer der erste Grund, warum die Stufe nicht höher liegt — mit der
/// Range, aus der er stammt; ab A2 stehen dort die geprüften Witness-Fakten.
pub(super) fn assurance_line(report: &AssuranceReport) -> String {
    let rating = &report.rating;
    // Der Reader entschärft Principals und Gründe schon; die zweite Schicht
    // an der Senke kostet nichts (wie `SignatureState::word`).
    let detail = crate::text::sanitize(&rating.detail());
    format!("Assurance      {:<LEVEL_WIDTH$}({detail})", rating.word())
}

/// `range 2: witness signature not checked — no trusted allowed_signers`
/// — der erste Grund, warum die Stufe nicht höher liegt, auf jeder Stufe
/// (für Gates und das Audit-Bündel; die `Assurance`-Zeile nennt ab A2 die
/// Witness-Fakten).
///
/// Ein Grund, der die ganze Session trifft (veränderte Seals, offene Kette,
/// Ledger, fremde Signaturbefunde), steht ohne Range: Die Nummer zählte nur
/// die lesbaren Seals und zeigte auf eine Zeile, die gar nicht der Grund
/// ist.
pub(crate) fn first_reason(report: &AssuranceReport) -> Option<String> {
    let first = report.reasons.first()?;
    let reason = first.text();
    Some(match report.weakest {
        Some(index) if !first.is_session_wide() => format!("range {}: {reason}", index + 1),
        _ => reason,
    })
}

/// Die `Intent`-Zeile des Session-Blocks: die Signaturlage, dann der Anker
/// (gekürzt) und was über seine Bindung feststeht —
/// `Intent         intent signed, software key (b3-7a41c2d9…, unchained)`.
///
/// Ein Assurance-Fakt, kein Verdikt: Eine fehlende oder ungültige
/// Signatur ändert weder Integrität noch Exit-Code (W6).
pub(super) fn intent_line(report: &AssuranceReport) -> String {
    let phrase = report.intent.phrase();
    let IntentState::Bound {
        anchor_id,
        chained,
        snapshot_matches,
        changed_mid_session,
        ..
    } = &report.intent
    else {
        return format!("Intent         {phrase}");
    };
    let mut facts = vec![
        format!(
            "{}…",
            anchor_id.as_str().get(..11).unwrap_or(anchor_id.as_str())
        ),
        if *chained { "chained" } else { "unchained" }.to_owned(),
    ];
    if *changed_mid_session {
        facts.push(Reason::IntentChangedMidSession.text());
    }
    if !snapshot_matches {
        facts.push(Reason::IntentSnapshotMismatch.text());
    }
    format!("Intent         {phrase} ({})", facts.join(", "))
}

/// Die `Not proven`-Zeile(n) für die erreichte Stufe: die Kurzformen, mit
/// ` · ` verbunden, höchstens drei, dann der Verweis auf `--limits`; mit
/// `full` (`--limits`) jeder Satz ausgeschrieben. Hat der Witness das Ende
/// der Session nicht gesehen ([`SessionEnd::Open`]), steht das als
/// letzter Satz dabei — eine Information neben der Stufe, kein Abzug
/// (EA-11: A2 verlangt Ruhe, kein `SessionEnd`).
pub(super) fn not_proven_lines(
    level: AssuranceLevel,
    full: bool,
    session_end: SessionEnd,
) -> Vec<String> {
    let mut limits: Vec<_> = limits_at(level.level()).collect();
    if session_end == SessionEnd::Open {
        limits.push(&minds_core::evidence::SESSION_END_NOT_WITNESSED);
    }
    if full {
        let mut lines = vec![format!(
            "Not proven     {} limit(s) at {}",
            limits.len(),
            level.word()
        )];
        lines.extend(limits.iter().map(|limit| format!("  - {}", limit.text)));
        return lines;
    }
    let shown: Vec<&str> = limits
        .iter()
        .take(NOT_PROVEN_SHORT)
        .map(|limit| limit.short)
        .collect();
    let more = if limits.len() > NOT_PROVEN_SHORT {
        " (minds verify --limits)"
    } else {
        ""
    };
    vec![format!("Not proven     {}{more}", shown.join(" · "))]
}

/// Die Gate-Zeile, falls die schwächste Stufe unter `required` liegt.
/// Ohne bewertete Session ist das Gate nicht bestanden (fail-closed).
pub(crate) fn gate_failure(levels: &[AssuranceLevel], required: AssuranceLevel) -> Option<String> {
    match levels.iter().min() {
        None => Some(format!(
            "Gate           assurance not assessed (no session) — required {}",
            required.word()
        )),
        Some(lowest) if *lowest < required => Some(format!(
            "Gate           assurance {} < required {}",
            lowest.word(),
            required.word()
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minds_core::evidence::WitnessProfile;
    use minds_reader::assurance::{IntentSignature, SealSignature, SignerKind, WitnessStart};

    fn h(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    /// `closes`: Der Bereich ruht und schließt die Session; sonst weder.
    fn range(byte: u8, scope: &str, signature: SealSignature, closes: bool) -> RangeInput {
        RangeInput {
            seal: h(byte),
            scope: scope.into(),
            signature,
            quiescence: closes.then_some(minds_core::Quiescence::Quiescent),
            closes_session: closes,
        }
    }

    fn witness() -> SealSignature {
        SealSignature::Witness {
            principal: "minds-witness-user@build-07".into(),
        }
    }

    /// Ein bezeugter Bereich mit allem, was A2 verlangt — aus Material ist
    /// das heute nicht erreichbar (Modul-Doku), die Zeile muss trotzdem
    /// stehen.
    fn a2_report() -> AssuranceReport {
        let ranges = [range(1, SCOPE_WITNESS_V1, witness(), true)];
        let starts = [WitnessStart {
            profile: Some(WitnessProfile::User),
            isolated: true,
            key: None,
        }];
        let intent = IntentState::Bound {
            anchor_id: ContentHash::from_bytes([0xa1; 32]),
            chained: true,
            signature: IntentSignature::Valid(SignerKind::SecurityKey),
            snapshot_matches: true,
            from_session_start: true,
            changed_mid_session: false,
        };
        assess(&AssuranceInput {
            seals: Seals::Sealed(&ranges),
            integrity: EvidenceVerdict::Verified,
            chain_closed: true,
            ledger: &LedgerCheck::Complete,
            link: Some(EvidenceSource::Observed),
            trusted_signers: true,
            observations: &FsCoverage::Complete,
            witness_starts: &starts,
            intent: &intent,
            replay: None,
            anchors: None,
        })
    }

    #[test]
    fn verify_assurance_golden_a2_clean() {
        let report = a2_report();
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        let mut lines = vec![assurance_line(&report)];
        lines.extend(not_proven_lines(report.overall, false, report.session_end));
        assert_eq!(
            lines.join("\n"),
            "Assurance      A2 witnessed   (minds-witness-user@build-07, profile user; intent signed, sk key)\n\
             Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)"
        );
    }

    /// A2 ohne Sessionende (EA-11): Der Bereich ruht (`Stop`), schließt aber
    /// nicht — die Stufe bleibt A2, und `Not proven` nennt das Ende als
    /// letzten Satz, kurz wie lang.
    #[test]
    fn verify_assurance_a2_without_session_end_names_it_under_not_proven() {
        let ranges = [RangeInput {
            closes_session: false,
            ..range(1, SCOPE_WITNESS_V1, witness(), true)
        }];
        let starts = [WitnessStart {
            profile: Some(WitnessProfile::User),
            isolated: true,
            key: None,
        }];
        let intent = IntentState::Bound {
            anchor_id: ContentHash::from_bytes([0xa1; 32]),
            chained: true,
            signature: IntentSignature::Valid(SignerKind::SecurityKey),
            snapshot_matches: true,
            from_session_start: true,
            changed_mid_session: false,
        };
        let report = assess(&AssuranceInput {
            seals: Seals::Sealed(&ranges),
            integrity: EvidenceVerdict::Verified,
            chain_closed: true,
            ledger: &LedgerCheck::Complete,
            link: Some(EvidenceSource::Observed),
            trusted_signers: true,
            observations: &FsCoverage::Complete,
            witness_starts: &starts,
            intent: &intent,
            replay: None,
            anchors: None,
        });
        assert_eq!(report.overall, AssuranceLevel::A2Witnessed);
        assert_eq!(report.session_end, SessionEnd::Open);
        assert_eq!(
            assurance_line(&report),
            "Assurance      A2 witnessed   (minds-witness-user@build-07, profile user; intent signed, sk key)"
        );
        let short = not_proven_lines(report.overall, false, report.session_end);
        assert_eq!(
            short,
            [
                "Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)"
            ]
        );
        let full = not_proven_lines(report.overall, true, report.session_end);
        let count = limits_at(AssuranceLevel::A2Witnessed.level()).count() + 1;
        assert_eq!(
            full[0],
            format!("Not proven     {count} limit(s) at A2 witnessed")
        );
        assert_eq!(
            full.last().map(String::as_str),
            Some(
                "  - Not that the session has ended: the last sealed range ends at a stop (the agent had finished answering), but no SessionEnd was witnessed — later events run into the next epoch, which the next checkpoint seals."
            )
        );
        // Mit bezeugtem Ende fehlt der Satz.
        let full = not_proven_lines(report.overall, true, SessionEnd::Witnessed);
        assert_eq!(full.len(), count);
    }

    #[test]
    fn verify_assurance_golden_a1_untrusted_signers() {
        let ranges = [
            range(1, "agent-hooks/v1", SealSignature::NotChecked, false),
            range(2, SCOPE_WITNESS_V1, SealSignature::NotChecked, true),
        ];
        let report = assess(&AssuranceInput {
            seals: Seals::Sealed(&ranges),
            integrity: EvidenceVerdict::Verified,
            chain_closed: true,
            ledger: &LedgerCheck::NotChecked,
            link: Some(EvidenceSource::Observed),
            trusted_signers: false,
            observations: &FsCoverage::Unavailable { cause: None },
            witness_starts: &[],
            intent: &IntentState::Unbound,
            replay: None,
            anchors: None,
        });
        // Gleichstand bei A1: die erste Range ist die schwächste.
        assert_eq!(
            assurance_line(&report),
            "Assurance      A1 observed    (range 1: observed by the agent's hooks only (scope agent-hooks/v1))"
        );
        let witness_only = [range(2, SCOPE_WITNESS_V1, SealSignature::NotChecked, true)];
        let report = assess(&AssuranceInput {
            seals: Seals::Sealed(&witness_only),
            integrity: EvidenceVerdict::Verified,
            chain_closed: true,
            ledger: &LedgerCheck::NotChecked,
            link: Some(EvidenceSource::Observed),
            trusted_signers: false,
            observations: &FsCoverage::Unavailable { cause: None },
            witness_starts: &[],
            intent: &IntentState::Unbound,
            replay: None,
            anchors: None,
        });
        assert_eq!(
            assurance_line(&report),
            "Assurance      A1 observed    (range 1: witness signature not checked — no trusted allowed_signers)"
        );
    }

    #[test]
    fn legacy_sessions_name_their_reason_without_a_range() {
        let report = assess(&AssuranceInput {
            seals: Seals::Legacy,
            integrity: EvidenceVerdict::Incomplete,
            chain_closed: true,
            ledger: &LedgerCheck::NotChecked,
            link: None,
            trusted_signers: false,
            observations: &FsCoverage::Unavailable { cause: None },
            witness_starts: &[],
            intent: &IntentState::Unbound,
            replay: None,
            anchors: None,
        });
        assert_eq!(
            assurance_line(&report),
            "Assurance      A0 claimed     (captured before the evidence chain)"
        );
    }

    #[test]
    fn verify_limits_flag() {
        let short = not_proven_lines(AssuranceLevel::A1Observed, false, SessionEnd::Witnessed);
        assert_eq!(short.len(), 1);
        assert!(short[0].ends_with("(minds verify --limits)"), "{short:?}");
        let full = not_proven_lines(AssuranceLevel::A1Observed, true, SessionEnd::Witnessed);
        let count = limits_at(AssuranceLevel::A1Observed.level()).count();
        assert_eq!(
            full[0],
            format!("Not proven     {count} limit(s) at A1 observed")
        );
        assert_eq!(full.len(), count + 1);
        // A1 nennt die Lücke zwischen Anhängen und Versiegeln, A2 nicht mehr.
        let append = minds_core::evidence::DOES_NOT_PROVE_V2
            .iter()
            .find(|s| s.id == "append_to_seal_window")
            .unwrap()
            .text;
        assert!(full.iter().any(|line| line.ends_with(append)));
        let a2 = not_proven_lines(AssuranceLevel::A2Witnessed, true, SessionEnd::Witnessed);
        assert!(!a2.iter().any(|line| line.ends_with(append)));
    }

    #[test]
    fn tool_names_cannot_forge_columns() {
        assert_eq!(
            shown_tool("Write x  b3-00000000…  corroborated"),
            "Write_x__b3-00000000…__corroborated"
        );
        assert!(!shown_tool("Write\u{1b}[2K").contains('\u{1b}'));
        assert_eq!(shown_tool(&"W".repeat(50)).chars().count(), TOOL_CAP + 1);
    }

    #[test]
    fn the_gate_compares_the_weakest_session() {
        use AssuranceLevel::*;
        assert_eq!(
            gate_failure(&[A2Witnessed, A3Reproduced], A2Witnessed),
            None
        );
        assert_eq!(
            gate_failure(&[A2Witnessed, A1Observed], A2Witnessed).as_deref(),
            Some("Gate           assurance A1 observed < required A2 witnessed")
        );
        assert_eq!(gate_failure(&[A0Claimed], A0Claimed), None);
        assert!(gate_failure(&[], A0Claimed).is_some(), "fail-closed");
    }

    #[test]
    fn required_levels_parse_strictly() {
        assert_eq!(parse_required("A2"), Ok(AssuranceLevel::A2Witnessed));
        for bad in ["a2", "A4", "", "2", " A2", "A2 witnessed"] {
            assert!(parse_required(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ledger_lines_are_read_strictly_except_a_torn_tail() {
        let a = h(1).to_string();
        let b = h(2).to_string();
        let text = format!("{a} witness/v1 2026-10-02T10:00:00Z\n{b} witness-fs/v1 x\n{a} y z\n");
        let (ids, torn) = ledger_ids(&text).unwrap();
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), [h(1), h(2)]);
        assert!(!torn);
        // Abgerissenes Ende: nicht gezählt, kein Fehler — aber gemeldet.
        for tail in ["b3-12", b.as_str(), &format!("{b} witness/v1 2026")] {
            let (ids, torn) = ledger_ids(&format!("{a} witness/v1 t\n{tail}")).unwrap();
            assert_eq!(ids.len(), 1, "{tail}");
            assert!(torn, "{tail}");
        }
        // Eine unlesbare oder unvollständige Zeile mittendrin ist ein Fehler.
        assert!(ledger_ids(&format!("garbage\n{a} w t\n")).is_err());
        assert!(ledger_ids(&format!("{a}\n{b} w t\n")).is_err());
        assert!(ledger_ids(&format!("{a} w t extra\n")).is_err());
        let (ids, torn) = ledger_ids("").unwrap();
        assert!(ids.is_empty() && !torn);
    }

    #[test]
    fn ledger_findings_are_capped() {
        let ledger = Ledger {
            check: LedgerCheck::Missing((0..10).map(h).collect()),
            missing: (0..9).map(h).collect(),
            altered: vec![h(9)],
            torn: false,
        };
        let lines = ledger_findings(&ledger);
        assert_eq!(lines.len(), MAX_LISTED + 1);
        assert_eq!(lines[MAX_LISTED], "… 2 more");
        let one = Ledger {
            missing: Vec::new(),
            altered: vec![h(9)],
            ..ledger
        };
        assert_eq!(
            ledger_findings(&one),
            [format!("witnessed seal {} altered in the repository", h(9))]
        );
        assert!(ledger_findings(&Ledger::not_checked()).is_empty());
    }
}
