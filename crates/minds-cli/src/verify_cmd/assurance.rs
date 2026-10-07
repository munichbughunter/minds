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
//! - **Witness-Starts** (`witness.start`: Profil, Isolationsbeleg) liegen nur
//!   im Journal des Witness, nicht im Repository — kein Seal und kein
//!   Observation-Objekt trägt sie. Die Liste bleibt leer; A2 ist damit nicht
//!   erreichbar (`witness profile unknown`), fail-closed. Erreichbar wird es,
//!   sobald der Witness Profil und Trennung im signierten Material festhält.
//! - **Replay** (EA-18b) und **Gegenzeichnungen** (EA-19) gibt es noch
//!   nicht: keine, keine. Der **Intent** (EA-14/EA-15) wird gelesen
//!   ([`minds_reader::intent::intent_of`]); seine Signatur gilt nur unter
//!   `minds-intent` gegen dieselbe vertrauenswürdige Signer-Datei.
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
    Assurance, AssuranceInput, AssuranceReport, FsCoverage, IntentSignature, IntentState,
    LedgerCheck, MAX_LISTED, RangeInput, Reason, Seals, assess,
};
use minds_reader::model::EvidenceVerdict;
use minds_reader::observations::Window;
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
pub(crate) fn parse_required(raw: &str) -> Result<Assurance, String> {
    Assurance::parse(raw).ok_or_else(|| {
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
    /// Die Intent-Lage ([`intent_state`]) — einmal je Session gerechnet,
    /// damit Coverage (Scope-Befunde, EA-17) und Stufe denselben Anker
    /// sehen.
    pub intent: &'a IntentState,
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

    let integrity = if facts.tampered {
        EvidenceVerdict::Tampered
    } else if facts.complete {
        EvidenceVerdict::Verified
    } else {
        EvidenceVerdict::Incomplete
    };
    assess(&AssuranceInput {
        seals: if facts.legacy {
            Seals::Legacy
        } else {
            Seals::Sealed(&ranges)
        },
        integrity,
        chain_closed: facts.chain_closed,
        ledger: facts.ledger,
        link: facts.link,
        trusted_signers: trust.trusted(),
        observations: facts.observations,
        witness_starts: &[],
        intent: facts.intent,
        replay: None,
        anchors: None,
    })
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
    let witnessed = |seal_id: &ContentHash, _: &Seal| trust.witnessed(seal_id);
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

/// Ob der Bereich mit dem Ende der Session schließt: nur, wenn der Seal
/// genau die geprüfte Session nennt — deren Objekt hat der Verdikt-Pfad mit
/// Hash-Prüfung gelesen — und sie `lineage.closed` trägt. Alles andere
/// (Block-Seal, eine andere Session, nicht lesbar): `false`, fail-closed.
fn closes_session(facts: &Facts<'_>, seal: &Seal) -> bool {
    let SealOutcome::Stored { session } = &seal.outcome else {
        return false;
    };
    session.parse::<SessionId>().ok() == Some(facts.id)
        && facts
            .session
            .and_then(|session| session.lineage.as_ref())
            .is_some_and(|lineage| lineage.closed)
}

/// Die Abdeckung durch den Datei-Beobachter: lückenlos, wenn der Witness
/// ein Fenster über die Session trägt und keine seiner Epochen eine Lücke
/// zählt; ohne Fenster nicht verfügbar.
///
/// Die Lückengründe (EA-08c) stehen heute nicht im Material — eine Lücke
/// erscheint deshalb ohne Grund (`observation gap`).
pub(crate) fn fs_coverage(store: &dyn ContextStore, windows: &[Window]) -> FsCoverage {
    if windows.is_empty() {
        return FsCoverage::Unavailable { cause: None };
    }
    // Ein Fenster ohne benannte Epochen („jede vertrauenswürdige") belegt
    // keine lückenlose Kette — fail-closed.
    if windows.iter().any(|window| window.epochs.is_none()) {
        return FsCoverage::Unavailable { cause: None };
    }
    let gapped = windows
        .iter()
        .flat_map(|window| window.epochs.iter().flatten())
        .any(|epoch| {
            // Unlesbar heißt: nicht belegt lückenlos.
            store
                .seal_text(epoch)
                .ok()
                .flatten()
                .and_then(|text| Seal::parse(&text).ok())
                .is_none_or(|seal| seal.gaps > 0)
        });
    if gapped {
        FsCoverage::Gapped {
            reasons: Vec::new(),
        }
    } else {
        FsCoverage::Complete
    }
}

/// Die Schreib-Claims der Session, die der Witness nicht bestätigt — als
/// Detailzeilen unter der Coverage-Zeile, gekappt auf
/// [`super::artifact::DETAIL_CAP`] (außer mit `all`).
///
/// Nur mit Beobachtungsfenster: Ohne bezeugtes Fenster hat niemand
/// beobachtet, und „unbestätigt" wäre über jeden Claim wahr und damit
/// nichtssagend. Die Stelle ist Zug und Aufruf in der gespeicherten Session
/// (1-basiert) — die Event-`seq` des Journals steht nicht im Store.
pub(super) fn uncorroborated_lines(
    root: &Path,
    id: SessionId,
    session: &Session,
    observations: &[minds_reader::reconcile::FsObservation],
    all: bool,
) -> Vec<String> {
    use minds_reader::observations::{Corroboration, corroborations};
    let spellings = minds_reader::artifact::root_spellings(root);
    let roots: Vec<&Path> = spellings.iter().map(std::path::PathBuf::as_path).collect();
    let claims: Vec<String> = corroborations(&[session], &roots, observations)
        .into_iter()
        .filter(|claim| claim.corroboration == Corroboration::Uncorroborated)
        .filter_map(|claim| {
            let call = session.turns.get(claim.turn)?.tool_calls.get(claim.call)?;
            let written = call.effect.as_ref()?.written.as_ref()?.to_string();
            let path = super::artifact::shown_path(claim.path.as_deref()?);
            Some(format!(
                "  uncorroborated  turn {} call {}  {} {path}  {}…  no file-system observation",
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

/// Die `Assurance`-Zeile. Unter A2 steht in der Klammer der erste Grund,
/// warum die Stufe nicht höher liegt — mit der Range, aus der er stammt; ab
/// A2 stehen dort die geprüften Witness-Fakten.
pub(super) fn assurance_line(report: &AssuranceReport) -> String {
    let word = report.overall.word();
    let detail = if report.overall >= Assurance::A2Witnessed {
        witness_detail(report).or_else(|| first_reason(report))
    } else {
        first_reason(report)
    };
    match detail {
        Some(detail) => format!("Assurance      {word:<LEVEL_WIDTH$}({detail})"),
        None => format!("Assurance      {word}"),
    }
}

/// `range 2: witness signature not checked — no trusted allowed_signers`.
///
/// Ein Grund, der die ganze Session trifft (veränderte Seals, offene Kette,
/// Ledger, fremde Signaturbefunde), steht ohne Range: Die Nummer zählte nur
/// die lesbaren Seals und zeigte auf eine Zeile, die gar nicht der Grund
/// ist.
pub(crate) fn first_reason(report: &AssuranceReport) -> Option<String> {
    let first = report.reasons.first()?;
    let session_wide = matches!(
        first,
        Reason::IntegrityBroken
            | Reason::ChainOpen
            | Reason::LedgerSealsMissing { .. }
            | Reason::OtherSealsInvalid { .. }
    );
    let reason = first.text();
    Some(match report.weakest {
        Some(index) if !session_wide => format!("range {}: {reason}", index + 1),
        _ => reason,
    })
}

/// `minds-witness@build-07, profile container; intent signed, sk key`.
fn witness_detail(report: &AssuranceReport) -> Option<String> {
    let facts = report.witness.as_ref()?;
    // Der Reader entschärft die Principals schon; die zweite Schicht an der
    // Senke kostet nichts (wie `SignatureState::word`).
    let mut parts: Vec<String> = facts
        .principals
        .iter()
        .map(|principal| crate::text::sanitize(principal))
        .collect();
    if facts.principals_omitted > 0 {
        parts.push(format!("+{} more", facts.principals_omitted));
    }
    if !facts.profiles.is_empty() {
        let names: Vec<&str> = facts.profiles.iter().map(|p| p.name()).collect();
        parts.push(format!("profile {}", names.join("/")));
    }
    // Ab A2 ist der Intent verkettet (sonst `intent bound (unchained)` als
    // Grund) — die Signaturlage genügt.
    let intent = intent_phrase(&report.intent);
    Some(format!("{}; {intent}", parts.join(", ")))
}

/// Die Intent-Lage in festen Worten (EA-15): `intent not bound`, `intent
/// unsigned`, `intent signed, sk key`, `intent signed, software key`,
/// `intent signature not checked`, `intent signature invalid for
/// minds-intent`. Dieselben Sätze wie die Gründe des Readers.
fn intent_phrase(intent: &IntentState) -> String {
    match intent {
        IntentState::Unbound => Reason::IntentNotBound.text(),
        IntentState::Bound { signature, .. } => match signature {
            IntentSignature::Valid(kind) => format!("intent signed, {}", kind.word()),
            IntentSignature::Unsigned => Reason::IntentUnsigned.text(),
            IntentSignature::NotChecked => Reason::IntentSignatureNotChecked.text(),
            IntentSignature::Invalid => Reason::IntentSignatureInvalid.text(),
        },
    }
}

/// Die `Intent`-Zeile des Session-Blocks: die Signaturlage, dann der Anker
/// (gekürzt) und was über seine Bindung feststeht —
/// `Intent         intent signed, software key (b3-7a41c2d9…, unchained)`.
///
/// Ein Assurance-Fakt, kein Verdikt: Eine fehlende oder ungültige
/// Signatur ändert weder Integrität noch Exit-Code (W6).
pub(super) fn intent_line(report: &AssuranceReport) -> String {
    let phrase = intent_phrase(&report.intent);
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
/// `full` (`--limits`) jeder Satz ausgeschrieben.
pub(super) fn not_proven_lines(level: Assurance, full: bool) -> Vec<String> {
    let limits: Vec<_> = limits_at(level.level()).collect();
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
pub(crate) fn gate_failure(levels: &[Assurance], required: Assurance) -> Option<String> {
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
    use minds_reader::assurance::{SealSignature, SignerKind, WitnessStart};

    fn h(byte: u8) -> ContentHash {
        ContentHash::from_bytes([byte; 32])
    }

    fn range(byte: u8, scope: &str, signature: SealSignature, closes: bool) -> RangeInput {
        RangeInput {
            seal: h(byte),
            scope: scope.into(),
            signature,
            closes_session: closes,
        }
    }

    fn witness() -> SealSignature {
        SealSignature::Witness {
            principal: "minds-witness@build-07".into(),
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
        assert_eq!(report.overall, Assurance::A2Witnessed);
        let mut lines = vec![assurance_line(&report)];
        lines.extend(not_proven_lines(report.overall, false));
        assert_eq!(
            lines.join("\n"),
            "Assurance      A2 witnessed   (minds-witness@build-07, profile user; intent signed, sk key)\n\
             Not proven     model identity · correctness of the decision · actions outside the boundary (minds verify --limits)"
        );
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
        let short = not_proven_lines(Assurance::A1Observed, false);
        assert_eq!(short.len(), 1);
        assert!(short[0].ends_with("(minds verify --limits)"), "{short:?}");
        let full = not_proven_lines(Assurance::A1Observed, true);
        let count = limits_at(Assurance::A1Observed.level()).count();
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
        let a2 = not_proven_lines(Assurance::A2Witnessed, true);
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
        use Assurance::*;
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
        assert_eq!(parse_required("A2"), Ok(Assurance::A2Witnessed));
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
