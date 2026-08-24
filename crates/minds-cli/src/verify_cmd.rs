//! `minds verify` — vom Signatur-Check zum Evidence-Verdikt (ADR-0011).
//!
//! Drei Betriebsarten:
//!
//! - `minds verify <session>` — das **Evidence-Verdikt**: Integrität ×
//!   Coverage über die Seals der Session, als Matrix mit festen Exit-Codes.
//! - `minds verify <session> --sig <datei> [--signers] [--identity]` — der
//!   bisherige Attestation-Pfad: eine signierte Attribution prüfen.
//! - `minds verify --evidence <seal-id>` — das Verdikt eines einzelnen Seals,
//!   auch ohne Session (der Redaction-Block-Fall).
//!
//! # VALID ≠ COMPLETE
//!
//! Die zwei Achsen sind getrennte Urteile (ADR-0011, Entscheidung 7):
//!
//! | | Coverage vollständig | Coverage unvollständig/unbekannt |
//! |---|---|---|
//! | Integrität intakt | `VERIFIZIERT` | `VERIFIZIERT, UNVOLLSTÄNDIG` |
//! | Integrität verletzt | `MANIPULIERT` | `MANIPULIERT` |
//! | Kein Material | — | `NICHT VERIFIZIERBAR` |
//!
//! Exit-Codes (CI-Vertrag): **0** VERIFIZIERT · **1** MANIPULIERT ·
//! **2** VERIFIZIERT, UNVOLLSTÄNDIG · **3** NICHT VERIFIZIERBAR ·
//! **4** operativer Fehler (Store nicht lesbar, ssh-keygen fehlt, …) — ein
//! flakiger Runner darf nie als „manipuliert" durchgehen, deshalb kollidiert
//! der Fehlerpfad nicht mit Code 1.
//!
//! Eine Alt-Session ohne Seal ist ein **Zustand**, kein Fehler: Sie wurde vor
//! der Evidence-Chain erfasst; das Verdikt sagt genau das. Der heuristische
//! Epochen-Schluss über `lineage.local_id` erscheint nur als Hinweis und
//! wertet das Verdikt **nie** auf — Heuristik bleibt Heuristik.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode};

use minds_core::evidence::{Seal, SealOutcome};
use minds_core::{ContentHash, SessionId};
use minds_store::{ContextStore, StoreError};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Das Verdikt der Matrix, mit seinem Exit-Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Integrität intakt, Coverage vollständig.
    Verified,
    /// Integrität verletzt — unabhängig von der Coverage.
    Tampered,
    /// Integrität intakt, aber bekannte Lücken, offene Epochen oder eine
    /// zurückgewiesene Nutzlast.
    Incomplete,
    /// Kein Material, über das sich urteilen ließe.
    Unverifiable,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Verdict::Verified => "VERIFIZIERT",
            Verdict::Tampered => "MANIPULIERT",
            Verdict::Incomplete => "VERIFIZIERT, UNVOLLSTÄNDIG",
            Verdict::Unverifiable => "NICHT VERIFIZIERBAR",
        }
    }

    fn exit(self) -> ExitCode {
        match self {
            Verdict::Verified => ExitCode::from(0),
            Verdict::Tampered => ExitCode::from(1),
            Verdict::Incomplete => ExitCode::from(2),
            Verdict::Unverifiable => ExitCode::from(3),
        }
    }
}

/// Führt `minds verify` aus.
pub fn run(
    target: Option<&str>,
    sig: Option<&str>,
    signers: Option<&str>,
    identity: Option<&str>,
    evidence: Option<&str>,
) -> ExitCode {
    match (evidence, target, sig) {
        // Ein einzelner Seal, auch ohne Session.
        (Some(seal_id), None, None) => match verify_seal(seal_id, signers, identity) {
            Ok(verdict) => verdict.exit(),
            Err(err) => operational_failure(err.as_ref()),
        },
        (Some(_), _, _) => {
            eprintln!("minds verify: --evidence steht allein (ohne <session-id>/--sig)");
            ExitCode::FAILURE
        }
        // Der Attestation-Pfad, unverändert.
        (None, Some(target), Some(sig)) => match verify_attestation(target, sig, signers, identity)
        {
            Ok(true) => {
                println!("gültig");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("UNGÜLTIG");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("minds verify: {err}");
                ExitCode::FAILURE
            }
        },
        // Das Evidence-Verdikt einer Session.
        (None, Some(target), None) => match verify_session(target, signers, identity) {
            Ok(verdict) => verdict.exit(),
            Err(err) => operational_failure(err.as_ref()),
        },
        (None, None, _) => {
            eprintln!("minds verify: erwartet <session-id> oder --evidence <seal-id>");
            ExitCode::FAILURE
        }
    }
}

/// Der operative Fehlerpfad: Exit **4**, nie 1 — und der Fehlertext läuft
/// durch die Terminal-Härtung, weil er Repo-Inhalte zitieren kann (etwa den
/// Tombstone-Grund in der Display-Form von `StoreError::Forgotten`).
fn operational_failure(err: &dyn std::error::Error) -> ExitCode {
    eprintln!("minds verify: {}", crate::text::sanitize(&err.to_string()));
    ExitCode::from(4)
}

// ---------------------------------------------------------------------------
// Das Evidence-Verdikt einer Session
// ---------------------------------------------------------------------------

/// Ein gelesener Seal samt Signaturstatus — die Zwischenform der Prüfung.
struct CheckedSeal {
    id: ContentHash,
    seal: Seal,
    signature: SignatureState,
}

/// Was über die Signatur eines Seals sagbar ist.
enum SignatureState {
    /// Keine `seal.sig` — hash-valide, aber ohne Urheber-Bindung.
    Unsigned,
    /// Signatur liegt, aber ohne allowed_signers ist sie nur eine Behauptung.
    Unchecked,
    /// Gegen allowed_signers geprüft und gültig.
    Valid,
    /// Mit **explizit** genannter Identität geprüft und ungültig —
    /// Manipulation.
    Invalid,
    /// Mit der **geratenen** Identität (lokales `user.email`) nicht
    /// verifizierbar. Der Seal speichert keinen Principal; wer den Seal
    /// eines Kollegen prüft, rät hier systematisch falsch — das ist kein
    /// Manipulationsbeweis, sondern eine offene Zuordnung (ADR-0011: ein
    /// Prüfprimitive, das regulär falsch-positiv rauscht, ist wertlos).
    NotAttributable,
}

impl SignatureState {
    fn word(&self) -> &'static str {
        match self {
            SignatureState::Unsigned => "unsigniert",
            SignatureState::Unchecked => "signiert (ungeprüft — mit --signers prüfen)",
            SignatureState::Valid => "Signatur gültig",
            SignatureState::Invalid => "SIGNATUR UNGÜLTIG",
            SignatureState::NotAttributable => "signiert (nicht zuordenbar — --identity angeben)",
        }
    }
}

fn verify_session(
    target: &str,
    signers: Option<&str>,
    identity: Option<&str>,
) -> Fallible<Verdict> {
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("keine gültige Session-Id {target:?}: {err}"))?;
    let ctx = Context::open()?;

    println!("Session   {id}");

    // 1. Die Session selbst: vorhanden, vergessen (payload-freier Beweis
    //    bleibt) — oder manipuliert.
    let mut tampered = false;
    let mut payload_missing = false;
    let mut notes: Vec<String> = Vec::new();
    let mut lineage: Option<(String, String)> = None;
    // Die dritte Achse: Deutung. Ein unbekanntes Tool ist KEIN Integritäts-
    // und KEIN Coverage-Problem — es ist eine Deutungslücke, und sie bekommt
    // ihre eigene Zeile statt das Verdikt zu vermischen (ADR-0011).
    let mut interpretation: Option<(usize, usize)> = None; // (gedeutet, ungedeutet)
    match ctx.store.get(id) {
        Ok(Some(session)) => {
            if let Some(l) = &session.lineage {
                lineage = Some((session.agent.name.clone(), l.local_id.clone()));
            }
            // `capture: None` ist ein Vor-Chain-Zustand und zählt in KEINER
            // Achse mit — dieselbe Semantik wie im Reader (`◐`-Zählung).
            let calls: Vec<_> = session
                .turns
                .iter()
                .flat_map(|t| t.tool_calls.iter())
                .filter(|c| c.capture.is_some())
                .collect();
            let uninterpreted = calls
                .iter()
                .filter(|c| {
                    c.capture
                        .as_ref()
                        .is_some_and(|cap| cap.status == minds_core::CaptureStatus::Uninterpreted)
                })
                .count();
            interpretation = Some((calls.len() - uninterpreted, uninterpreted));
            println!("Payload   im Store (Schema {})", session.schema_version);
        }
        Ok(None) => payload_missing = true,
        Err(StoreError::Forgotten { reason, .. }) => {
            // Der Grund ist fremdbestimmter Repo-Inhalt — Terminal-Härtung
            // an der Senke (#116), wie in render/reader.
            println!(
                "Payload   vergessen ({}) — der Seal bleibt der Beweis",
                crate::text::sanitize(&reason)
            );
        }
        Err(StoreError::Corrupt { .. }) => {
            println!("Payload   MANIPULIERT — Inhalt hasht nicht auf seine Id");
            tampered = true;
        }
        Err(err) => return Err(err.into()),
    }

    // 2. Die Seals: erst der Rückverweis, dann — falls der fehlt — der
    //    Namensraum (der Rückverweis ist best-effort).
    let mut seal_ids = ctx.store.seals_of(id)?;
    if seal_ids.is_empty() {
        seal_ids = seals_naming(ctx.store.as_ref(), id)?;
        if !seal_ids.is_empty() {
            notes.push(
                "Seal-Rückverweis (evidence.json) fehlte — über den Namensraum gefunden".into(),
            );
        }
    }
    if seal_ids.is_empty() && !tampered {
        println!("Seals     keine — vor Evidence-Chain erfasst");
        println!("{}", Verdict::Unverifiable.word());
        return Ok(Verdict::Unverifiable);
    }

    let (checked, mut incomplete_reasons, seal_tampered) =
        check_seals(ctx.store.as_ref(), &seal_ids, signers, identity, &ctx.root)?;
    tampered |= seal_tampered;

    // 3. Jeder stored-Seal muss DIESE Session nennen — und wenn ein Seal
    //    `stored` sagt, muss die Nutzlast auch auffindbar sein (kein
    //    Tombstone, einfach weg): Sonst ist die Integritäts-Achse für den
    //    Payload nicht prüfbar, und „VERIFIZIERT" über-claimte.
    for c in &checked {
        if let SealOutcome::Stored { session } = &c.seal.outcome {
            if session != &id.to_string() {
                incomplete_reasons.push(format!("Seal {} nennt eine andere Session", c.id));
            } else if payload_missing {
                incomplete_reasons
                    .push("Seal sagt stored, aber der Payload liegt nicht in diesem Store".into());
                payload_missing = false; // einmal genügt
            }
        }
    }
    if payload_missing {
        notes.push("Payload liegt nicht in diesem Store".into());
    }

    // 4. Coverage: gap-frei je Seal + geschlossene Epochenkette.
    let complete = coverage_complete(ctx.store.as_ref(), &checked, &mut incomplete_reasons)?;

    for c in &checked {
        print_seal_line(c);
        if matches!(c.signature, SignatureState::Invalid) {
            tampered = true;
        }
    }
    for note in &notes {
        println!("Hinweis   {note}");
    }
    for reason in &incomplete_reasons {
        println!("Lücke     {reason}");
    }

    // 5. Heuristischer Epochen-Hinweis — wertet NIE auf.
    if !complete && !tampered {
        if let Some((agent, local_id)) = lineage {
            let siblings = sibling_sessions(ctx.store.as_ref(), id, &agent, &local_id)?;
            if siblings > 0 {
                println!(
                    "Hinweis   heuristisch: {siblings} weitere Session(s) derselben local_id \
                     gefunden — rekonstruierte Nähe, kein Beleg; das Verdikt bleibt unverändert"
                );
            }
        }
    }

    let verdict = if tampered {
        Verdict::Tampered
    } else if complete && incomplete_reasons.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Incomplete
    };

    // Die drei Vertrauensachsen, getrennt ausgesprochen: Integrität („wurde
    // es verändert?"), Coverage („wissen wir, ob etwas fehlt?" — immer
    // innerhalb der Beobachtungsgrenze) und Deutung („was bedeutet es?").
    // Das Gesamt-Verdikt und die Exit-Codes bleiben der CI-Vertrag aus
    // Integrität × Coverage; die Deutung wertet nie auf oder ab.
    println!(
        "Integrität {}",
        if tampered { "VERLETZT" } else { "intakt" }
    );
    let scopes: Vec<String> = {
        // Zweite Schicht neben der Parse-Härtung: Der Scope stammt aus dem
        // Repo — fremdbestimmt, also entschärft ausgeben.
        let mut s: Vec<String> = checked
            .iter()
            .map(|c| crate::text::sanitize(&c.seal.scope))
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    };
    let boundary = if scopes.is_empty() {
        String::new()
    } else {
        format!(
            " (Grenze: {} — Aktivität außerhalb ist nicht erfasst)",
            scopes.join(", ")
        )
    };
    println!(
        "Coverage   {}{boundary}",
        if tampered {
            "nicht bewertbar"
        } else if complete && incomplete_reasons.is_empty() {
            "vollständig innerhalb der Grenze"
        } else {
            "unvollständig"
        }
    );
    let interpretation_note = match interpretation {
        Some((_, 0)) => {
            println!("Deutung    vollständig");
            None
        }
        Some((done, open)) => {
            println!(
                "Deutung    teilweise — {open} von {} Tool-Aufruf(en) beobachtet, aber nicht gedeutet (◐)",
                done + open
            );
            Some(" — Deutung teilweise")
        }
        None => {
            println!("Deutung    nicht bewertbar (Payload nicht lesbar)");
            None
        }
    };
    println!(
        "Gesamt    {}{}",
        verdict.word(),
        interpretation_note.unwrap_or("")
    );
    Ok(verdict)
}

/// Liest und prüft die genannten Seals. Liefert die lesbaren Seals, die
/// Unvollständigkeits-Gründe und ob Manipulation vorliegt.
fn check_seals(
    store: &dyn ContextStore,
    seal_ids: &[ContentHash],
    signers: Option<&str>,
    identity: Option<&str>,
    root: &Path,
) -> Fallible<(Vec<CheckedSeal>, Vec<String>, bool)> {
    let mut checked = Vec::new();
    let mut reasons = Vec::new();
    let mut tampered = false;

    for id in seal_ids {
        let text = match store.seal_text(id) {
            Ok(Some(text)) => text,
            Ok(None) => {
                reasons.push(format!(
                    "Seal {id} ist verwiesen, liegt aber nicht im Store"
                ));
                continue;
            }
            Err(StoreError::SealMismatch { .. }) => {
                println!("Seal      {id}: MANIPULIERT — Text hasht nicht auf seine Id");
                tampered = true;
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        let seal = match Seal::parse(&text) {
            Ok(seal) => seal,
            Err(err) => {
                // Hash stimmt, Form nicht: ein Artefakt, das wir nie so
                // geschrieben hätten — der Ref wurde fremdbelegt.
                println!("Seal      {id}: MANIPULIERT — {err}");
                tampered = true;
                continue;
            }
        };
        let signature = signature_state(store, id, &text, signers, identity, root)?;
        checked.push(CheckedSeal {
            id: id.clone(),
            seal,
            signature,
        });
    }
    Ok((checked, reasons, tampered))
}

/// Prüft die Signatur eines Seals, so weit die Umgebung es hergibt.
///
/// Ohne allowed_signers wird **nicht** geraten: „signiert (ungeprüft)" ist
/// eine andere Aussage als „gültig" — dieselbe Trennung wie bei
/// `minds reviews` (fail-closed, #12).
fn signature_state(
    store: &dyn ContextStore,
    id: &ContentHash,
    text: &str,
    signers: Option<&str>,
    identity: Option<&str>,
    root: &Path,
) -> Fallible<SignatureState> {
    let Some(signature) = store.seal_signature(id)? else {
        return Ok(SignatureState::Unsigned);
    };
    let Some(signers) = resolve_signers_optional(signers, root) else {
        return Ok(SignatureState::Unchecked);
    };
    let explicit = identity.is_some();
    let Some(identity) = identity
        .map(str::to_string)
        .or_else(|| git_config(root, "user.email"))
    else {
        return Ok(SignatureState::Unchecked);
    };
    if !minds_attest::ssh_keygen_available() {
        return Ok(SignatureState::Unchecked);
    }
    match minds_attest::ssh_verify(text, &signature, Path::new(&signers), &identity)? {
        true => Ok(SignatureState::Valid),
        // Nur eine explizit genannte Identität macht aus „verifiziert nicht"
        // einen Manipulationsbefund; die geratene ist eine offene Zuordnung.
        false if explicit => Ok(SignatureState::Invalid),
        false => Ok(SignatureState::NotAttributable),
    }
}

/// Coverage vollständig ⇔ jeder Seal gap-frei und `stored`, und die
/// `previous`-Kette schließt sich — mit der Epochen-Semantik aus ADR-0011:
/// Epochen sind eigene Sessions, ein aufgelöster `stored`-Vorgänger schließt
/// die Kette, ein Block-Seal mit identischem Root ist ein Policy-Fix (keine
/// Lücke), alles Baumelnde bleibt offen.
fn coverage_complete(
    store: &dyn ContextStore,
    checked: &[CheckedSeal],
    reasons: &mut Vec<String>,
) -> Fallible<bool> {
    let mut complete = true;

    for c in checked {
        if c.seal.gaps > 0 {
            reasons.push(format!(
                "Seal {}: {} Lücke(n) im Bereich {}–{}",
                c.id, c.seal.gaps, c.seal.first_seq, c.seal.last_seq
            ));
            complete = false;
        }
        if c.seal.pre_chain > 0 {
            reasons.push(format!(
                "Seal {}: {} Event(s) vor Evidence-Chain erfasst (ungebunden)",
                c.id, c.seal.pre_chain
            ));
            complete = false;
        }
        if matches!(c.seal.outcome, SealOutcome::Rejected) {
            reasons.push(format!(
                "Seal {}: Nutzlast durch Speicher-Policy zurückgewiesen",
                c.id
            ));
            complete = false;
        }
    }

    // Epochenkette: Epochen sind per Design EIGENE Sessions (ADR-0011 E2) —
    // ein `previous`, das auf einen auflösbaren, hash-validen stored-Seal
    // führt, SCHLIESST die Kette (sie setzt sich in der Vorgänger-Session
    // fort). Ein Block-Seal als Vorgänger ist nur dann Geschichte, wenn sein
    // Root identisch ist (Policy-Fix: dieselben Events wurden später doch
    // gespeichert); sonst eine zurückgewiesene Epoche. Baumelnde oder
    // unlesbare Vorgänger bleiben offen. Innerhalb der Menge: eine Linie,
    // kein Fork.
    let in_set: BTreeMap<&ContentHash, &CheckedSeal> = checked.iter().map(|c| (&c.id, c)).collect();
    let mut entry_points = 0usize;
    let mut internal_targets: std::collections::BTreeSet<&ContentHash> =
        std::collections::BTreeSet::new();
    for c in checked {
        match &c.seal.previous {
            None => entry_points += 1,
            Some(prev) if in_set.contains_key(prev) => {
                if !internal_targets.insert(prev) {
                    reasons.push(format!(
                        "Epochen-Fork: mehrere Seals setzen auf {prev} auf — Reihenfolge nicht belegt"
                    ));
                    complete = false;
                }
            }
            Some(prev) => match store.seal_text(prev) {
                Ok(Some(text)) => match Seal::parse(&text) {
                    Ok(prev_seal) => match &prev_seal.outcome {
                        SealOutcome::Stored { .. } => entry_points += 1,
                        SealOutcome::Rejected if prev_seal.root == c.seal.root => {
                            entry_points += 1;
                        }
                        SealOutcome::Rejected => {
                            reasons.push(format!(
                                "Epoche vor Seal {} wurde zurückgewiesen (Block-Seal {prev})",
                                c.id
                            ));
                            complete = false;
                        }
                    },
                    Err(_) => {
                        reasons.push(format!("Vorgänger-Seal {prev} ist nicht lesbar"));
                        complete = false;
                    }
                },
                Ok(None) => {
                    reasons.push(format!(
                        "Vorgänger-Seal {prev} liegt nicht im Store — Epochenkette offen"
                    ));
                    complete = false;
                }
                Err(StoreError::SealMismatch { .. }) => {
                    reasons.push(format!("Vorgänger-Seal {prev} wurde verändert"));
                    complete = false;
                }
                Err(err) => return Err(err.into()),
            },
        }
    }
    if !checked.is_empty() && entry_points != 1 && complete {
        reasons.push(format!(
            "Epochenkette hat {entry_points} Anfänge statt einem — Reihenfolge nicht belegt"
        ));
        complete = false;
    }

    Ok(complete && !checked.is_empty())
}

/// Fallback, wenn der Rückverweis fehlt: alle Seals des Namensraums lesen und
/// die behalten, deren `session=`-Zeile diese Session nennt.
fn seals_naming(store: &dyn ContextStore, id: SessionId) -> Fallible<Vec<ContentHash>> {
    let mut found = Vec::new();
    for seal_id in store.list_seals()? {
        let Ok(Some(text)) = store.seal_text(&seal_id) else {
            continue;
        };
        let Ok(seal) = Seal::parse(&text) else {
            continue;
        };
        if let SealOutcome::Stored { session } = &seal.outcome {
            if session == &id.to_string() {
                found.push(seal_id);
            }
        }
    }
    Ok(found)
}

/// Wie viele **andere** Sessions dieselbe `(agent, local_id)` tragen — der
/// heuristische Epochen-Hinweis.
fn sibling_sessions(
    store: &dyn ContextStore,
    this: SessionId,
    agent: &str,
    local_id: &str,
) -> Fallible<usize> {
    let mut count = 0;
    for id in store.list()? {
        if id == this {
            continue;
        }
        let Ok(Some(session)) = store.get(id) else {
            continue;
        };
        if session.agent.name == agent
            && session
                .lineage
                .as_ref()
                .is_some_and(|l| l.local_id == local_id)
        {
            count += 1;
        }
    }
    Ok(count)
}

fn print_seal_line(c: &CheckedSeal) {
    let outcome = match &c.seal.outcome {
        SealOutcome::Stored { .. } => "stored",
        SealOutcome::Rejected => "zurückgewiesen",
    };
    println!(
        "Seal      {}: seq {}–{}, {} Event(s), {} Lücke(n), {} — {}",
        c.id,
        c.seal.first_seq,
        c.seal.last_seq,
        c.seal.events,
        c.seal.gaps,
        outcome,
        c.signature.word()
    );
}

// ---------------------------------------------------------------------------
// Ein einzelner Seal (--evidence)
// ---------------------------------------------------------------------------

fn verify_seal(target: &str, signers: Option<&str>, identity: Option<&str>) -> Fallible<Verdict> {
    let id: ContentHash = target
        .parse()
        .map_err(|err| format!("keine gültige Seal-Id {target:?}: {err}"))?;
    let ctx = Context::open()?;

    let text = match ctx.store.seal_text(&id) {
        Ok(Some(text)) => text,
        Ok(None) => {
            println!("Seal      {id}: liegt nicht im Store");
            println!("{}", Verdict::Unverifiable.word());
            return Ok(Verdict::Unverifiable);
        }
        Err(StoreError::SealMismatch { .. }) => {
            println!("Seal      {id}: Text hasht nicht auf seine Id");
            println!("{}", Verdict::Tampered.word());
            return Ok(Verdict::Tampered);
        }
        Err(err) => return Err(err.into()),
    };
    let seal = match Seal::parse(&text) {
        Ok(seal) => seal,
        Err(err) => {
            println!("Seal      {id}: {err}");
            println!("{}", Verdict::Tampered.word());
            return Ok(Verdict::Tampered);
        }
    };
    let signature = signature_state(ctx.store.as_ref(), &id, &text, signers, identity, &ctx.root)?;
    let checked = CheckedSeal {
        id,
        seal,
        signature,
    };
    print_seal_line(&checked);

    if matches!(checked.signature, SignatureState::Invalid) {
        println!("{}", Verdict::Tampered.word());
        return Ok(Verdict::Tampered);
    }
    let verdict = match &checked.seal.outcome {
        SealOutcome::Rejected => {
            println!(
                "Hinweis   Nutzlast durch Speicher-Policy zurückgewiesen — der Seal ist der Beweis, dass der Bereich existierte"
            );
            Verdict::Incomplete
        }
        SealOutcome::Stored { session } => {
            println!("Session   {session}");
            // Dieselbe Ketten-Logik wie beim Session-Verdikt: ein extern
            // aufgelöster stored-Vorgänger (oder ein Policy-Fix-Block-Seal
            // mit identischem Root) ist keine Lücke.
            let mut reasons = Vec::new();
            let complete = coverage_complete(
                ctx.store.as_ref(),
                std::slice::from_ref(&checked),
                &mut reasons,
            )?;
            for reason in &reasons {
                println!("Lücke     {reason}");
            }
            if complete {
                Verdict::Verified
            } else {
                Verdict::Incomplete
            }
        }
    };
    println!("{}", verdict.word());
    Ok(verdict)
}

// ---------------------------------------------------------------------------
// Der Attestation-Pfad (unverändert)
// ---------------------------------------------------------------------------

fn verify_attestation(
    target: &str,
    sig_file: &str,
    signers: Option<&str>,
    identity: Option<&str>,
) -> Fallible<bool> {
    if !minds_attest::ssh_keygen_available() {
        return Err("ssh-keygen nicht gefunden".into());
    }
    let id: SessionId = target
        .parse()
        .map_err(|err| format!("keine gültige Session-Id {target:?}: {err}"))?;

    let ctx = Context::open()?;
    let session = ctx
        .store
        .get(id)?
        .ok_or_else(|| format!("Session {id} liegt nicht im Store"))?;

    let payload = minds_core::attestation_payload(id, &session)?;
    let signature = std::fs::read_to_string(sig_file)
        .map_err(|err| format!("Signaturdatei {sig_file:?} nicht lesbar: {err}"))?;
    let signers = resolve_signers(signers, &ctx.root)?;
    let identity = resolve_identity(identity, &ctx.root)?;

    Ok(minds_attest::ssh_verify(
        &payload,
        &signature,
        Path::new(&signers),
        &identity,
    )?)
}

/// Die allowed_signers-Datei: `--signers`, sonst `git config
/// gpg.ssh.allowedSignersFile`, sonst `~/.ssh/allowed_signers`.
fn resolve_signers(signers: Option<&str>, root: &Path) -> Fallible<String> {
    resolve_signers_optional(signers, root).ok_or_else(|| {
        "keine allowed_signers-Datei: --signers <datei> angeben oder \
         `git config gpg.ssh.allowedSignersFile` setzen"
            .into()
    })
}

/// Wie [`resolve_signers`], aber ohne Fehler — für Pfade, auf denen „nicht
/// prüfbar" eine gültige Antwort ist.
fn resolve_signers_optional(signers: Option<&str>, root: &Path) -> Option<String> {
    if let Some(signers) = signers {
        return Some(signers.to_string());
    }
    if let Some(configured) = git_config(root, "gpg.ssh.allowedSignersFile") {
        return Some(configured);
    }
    if let Ok(home) = std::env::var("HOME") {
        let default = format!("{home}/.ssh/allowed_signers");
        if Path::new(&default).exists() {
            return Some(default);
        }
    }
    None
}

/// Die Identität (Principal in allowed_signers): `--identity`, sonst
/// `git config user.email`.
fn resolve_identity(identity: Option<&str>, root: &Path) -> Fallible<String> {
    if let Some(identity) = identity {
        return Ok(identity.to_string());
    }
    git_config(root, "user.email").ok_or_else(|| {
        "keine Identität: --identity <id> angeben oder `git config user.email` setzen".into()
    })
}

fn git_config(root: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(value)
}
