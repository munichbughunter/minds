//! C12.1 · Evidence v2: ein Satz für Menschen (je Achse ein Satzteil, kein
//! Gesamturteil), das Urteil von `minds verify` in seinen Wörtern, genau
//! drei Achsen, LIMITS als Satz, die Technik hinter Space.

use super::*;
use crate::app::Tab;

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 30), (160, 40)];

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Fußzeile 1 — die vorletzte Zeile des Puffers.
fn footer_line(out: &str) -> String {
    out.lines().rev().nth(1).expect("footer").to_string()
}

/// Die Session-Zeile, die `minds verify` zu `c` liefert.
fn assurance(c: char) -> crate::SessionAssurance {
    crate::SessionAssurance {
        session: sid(c),
        level: minds_reader::assurance::Assurance::A1Observed,
        reason: None,
        intent: minds_reader::assurance::IntentState::Unbound,
        tampered: false,
    }
}

/// Ein Urteil über die gezeigte Session a.
fn check(verdict: Result<crate::VerifyVerdict, String>) -> crate::CommitVerify {
    crate::CommitVerify {
        verdict,
        sessions: vec![assurance('a')],
        out_of_scope: None,
        out_of_scope_paths: Vec::new(),
        scope_note: None,
        conformance: None,
    }
}

/// Die abgeglichene Fixture, geöffnet aus der Übersicht — mit dem Urteil
/// `verdict` für den Commit, wenn eines gegeben ist (geprüft um 10:00Z).
fn evidence_from_overview(
    verdict: Option<Result<crate::VerifyVerdict, String>>,
) -> (tempfile::TempDir, App<'static>) {
    evidence_with(verdict.map(check))
}

/// Wie [`evidence_from_overview`], mit einem fertigen `CommitVerify`.
fn evidence_with(check: Option<crate::CommitVerify>) -> (tempfile::TempDir, App<'static>) {
    let (dir, repo, inspection) = reconciled(false);
    let repo: &'static Repo = Box::leak(Box::new(repo));
    let mut app = App::new(inspection, repo, None);
    app.open_overview();
    let head = app.inspection.index().commits_of(sid('a'))[0];
    if let Some(check) = check {
        let state = app.overview.as_mut().unwrap();
        state.checks.insert(head, Ok(check));
        state.checked_at.insert(head, 10 * 3_600);
    }
    app.reduce(Action::Evidence);
    assert!(matches!(app.top(), Some(View::Evidence { .. })));
    (dir, app)
}

/// Eine manipulierte Session (Seal-Material verändert), ohne Prüfung,
/// aus der Sessions-Liste geöffnet.
fn tampered_without_a_check() -> (tempfile::TempDir, App<'static>) {
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), session("Task a", "2026-07-25T14:10:00Z"));
    let (seal_id, seal) = clean_seal(sid('a'));
    let index = Index::from_parts(sessions, BTreeMap::new())
        .with_seals(sid('a'), vec![(seal_id, seal, false)])
        .with_tampered_seal(sid('a'));
    let (dir, repo) = repo();
    let repo: &'static Repo = Box::leak(Box::new(repo));
    let mut app = App::new(Inspection::from_index(index, vec![], "repo"), repo, None);
    app.reduce(Action::Evidence);
    assert!(matches!(app.top(), Some(View::Evidence { .. })));
    (dir, app)
}

/// Prüfte die CLI andere Sessions als die gezeigte, gilt das Urteil nicht:
/// nie VERIFIED ohne Signaturprüfung **dieser** Session. (Gegenstück zu
/// `a_different_session_set_is_not_checked` in der Übersicht.)
#[test]
fn c121_a_verdict_over_other_sessions_is_not_shown_as_this_sessions() {
    let mut other = check(Ok(crate::VerifyVerdict::Verified));
    other.sessions = vec![assurance('e')];
    let (_dir, mut app) = evidence_with(Some(other));
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains(
            "Verdict of minds verify: not checked — the checked sessions differ from the shown ones"
        ),
        "{out}"
    );
    assert!(!out.contains("VERIFIED"), "{out}");
    let line = footer_line(&out);
    assert!(line.contains("not checked"), "{line}");
    assert!(!line.contains("VERIFIED"), "{line}");
}

/// Manipuliertes Seal-Material ohne gelaufene Prüfung: TAMPERED, laut —
/// und nie VERIFIED, nie „not yet verified" (das wäre zu leise).
#[test]
fn c121_tampered_without_a_check_is_tampered_never_verified() {
    let (_dir, mut app) = tampered_without_a_check();
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("Verdict of minds verify: ✗ TAMPERED — seal material altered · v: run verify"),
        "{out}"
    );
    assert!(!out.contains("VERIFIED"), "{out}");
    assert!(!out.contains("not yet verified"), "{out}");
    let line = footer_line(&out);
    assert!(line.contains("✗ TAMPERED"), "{line}");
}

/// Aufgeklappte Technik bei 80×24: kein Panic, `? help` und die Achsen
/// bleiben auf dem Schirm.
#[test]
fn c121_unfolded_technical_still_fits_80x24() {
    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Verified)));
    app.reduce(Action::Space);
    let out = render_sized(&mut app, 80, 24);
    for want in [
        "? help",
        "INTEGRITY",
        "COVERAGE",
        "INTERPRETATION",
        "TECHNICAL",
    ] {
        assert!(out.contains(want), "{want} in {out}");
    }
}

#[test]
fn c121_the_head_is_one_sentence_per_axis_and_no_overall_verdict() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("EVIDENCE · session b3-aaaaaaaa"), "{out}");
    // Je Achse ein Satzteil: Integrität, Coverage, Abgleich, Deutung.
    // Vor der Prüfung „sealed", nicht „intact" (Klasse 2: alt
    // `is intact and unsigned` → neu `is sealed and unsigned`; intact erst
    // mit dem Urteil von `minds verify`).
    assert!(
        out.contains("This session's record is sealed and unsigned"),
        "{out}"
    );
    assert!(!out.contains("record is intact"), "{out}");
    assert!(out.contains("events lie inside the hook boundary"), "{out}");
    assert!(out.contains("The 4 added lines are reported"), "{out}");
    assert!(out.contains("none were observed by a witness."), "{out}");
    assert!(out.contains("Every tool call was interpreted."), "{out}");
    // Kein Gesamturteil, kein „Cryptographically verified" — das wäre die
    // Verwechslung sealed ≠ verified.
    assert!(!out.contains("Cryptographically verified"), "{out}");
    assert!(!out.contains("EVIDENCE b3-aaaaaaaa"), "{out}");
}

#[test]
fn c121_without_a_check_the_verdict_says_not_yet_verified_never_verified() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("sealed, not yet verified — v: run verify"),
        "{out}"
    );
    // Kein VERIFIED ohne Signaturprüfung — nirgends auf dem Schirm.
    assert!(!out.contains("VERIFIED"), "{out}");
    assert!(!out.contains("checked "), "{out}");
}

#[test]
fn c121_the_verdict_is_the_one_of_minds_verify_with_its_time() {
    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Verified)));
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("Verdict of minds verify: ✓ VERIFIED (signatures checked 10:00Z)"),
        "{out}"
    );
    assert!(out.contains("checked 10:00Z"), "{out}");

    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Incomplete)));
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("Verdict of minds verify: ! VERIFIED, INCOMPLETE"),
        "{out}"
    );

    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Tampered)));
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("Verdict of minds verify: ✗ TAMPERED"), "{out}");

    let (_dir, mut app) = evidence_from_overview(Some(Err("no minds binary".into())));
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("Verdict of minds verify: not checked — no minds binary"),
        "{out}"
    );
    assert!(!out.contains("VERIFIED"), "{out}");
}

#[test]
fn c121_exactly_three_axes_with_the_old_sections_folded_in() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    let row = |label: &str| {
        out.lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("{label} row in {out}"))
            .to_string()
    };
    // INTEGRITY trägt Seals, Signatur, Kette und Stufe in einem — vor der
    // Prüfung als `◈ record sealed` (Klasse 2: alt `✓ record intact` ohne
    // Urteil → neu `◈ record sealed`; ✓ erst mit VERIFIED).
    let integrity = row("INTEGRITY");
    assert!(
        integrity.contains("◈ record sealed · 1 seal, unsigned · chain closed (1 epoch) · A1"),
        "{integrity}"
    );
    assert!(!integrity.contains("✓ record intact"), "{integrity}");
    // COVERAGE: die Zahlen, darunter der Abgleich mit den Wörtern aus C9.4.
    let coverage = row("COVERAGE");
    assert!(
        coverage.contains("complete · 4 events · 0 gaps"),
        "{coverage}"
    );
    assert!(
        out.contains("◇ 4 of 4 added lines reported, 0 observed"),
        "{out}"
    );
    let interpretation = row("INTERPRETATION");
    assert!(
        interpretation.contains("✓ all tool calls interpreted"),
        "{interpretation}"
    );
    // Die alten sieben Sektionen gibt es nicht mehr — und kein drittes Wort.
    for gone in [
        "EPOCHS",
        "SIGNATURE",
        "ARTIFACT",
        "VERDICT",
        "explained",
        "hash-valid",
    ] {
        assert!(!out.contains(gone), "{gone} in {out}");
    }
}

#[test]
fn c121_limits_is_a_sentence_for_the_level_not_a_count() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("LIMITS"), "{out}");
    assert!(out.contains("observed at A1 — no witness ran"), "{out}");
    assert!(!out.contains("named limits"), "{out}");
}

#[test]
fn c121_technical_is_folded_and_space_unfolds_the_unchanged_lines() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("TECHNICAL (for auditors)"), "{out}");
    assert!(out.contains("▸ Space"), "{out}");
    assert!(!out.contains("blake3"), "{out}");
    assert!(!out.contains("Chain root"), "{out}");

    app.reduce(Action::Space);
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("blake3 · derive_key, contexts minds/evidence/v1/*"),
        "{out}"
    );
    assert!(out.contains("Seal"), "{out}");
    assert!(out.contains("root"), "{out}");
    assert!(
        out.contains("✓ Externally verifiable: seal identity (seal_id = hash of the seal text) and signature."),
        "{out}"
    );
    assert!(
        out.contains(
            "Chain root: reproducible only locally with journal + session salt (anti-oracle)."
        ),
        "{out}"
    );
    assert!(!out.contains("▸ Space"), "{out}");

    app.reduce(Action::Space);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("blake3"), "{out}");
}

#[test]
fn c121_the_footer_names_id_strip_and_verdict_without_sentences() {
    let (_dir, mut app) = evidence_from_overview(None);
    let line = footer_line(&render_sized(&mut app, 160, 40));
    assert!(line.contains("b3-aaaaaaaa"), "{line}");
    assert!(line.contains("◈ sealed"), "{line}");
    assert!(line.contains("sealed, not yet verified"), "{line}");
    assert!(!line.contains(". "), "no sentence: {line}");
    assert!(!line.contains("Cryptographically"), "{line}");

    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Verified)));
    let line = footer_line(&render_sized(&mut app, 160, 40));
    assert!(line.contains("✓ VERIFIED"), "{line}");
    assert!(!line.contains("signatures checked"), "{line}");
    assert!(!line.contains(". "), "no sentence: {line}");
}

#[test]
fn c121_fits_80_120_160_with_help_and_the_three_axes() {
    for (w, h) in SIZES {
        let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Verified)));
        let out = render_sized(&mut app, w, h);
        for want in [
            "? help",
            "q quit",
            "INTEGRITY",
            "COVERAGE",
            "INTERPRETATION",
            "LIMITS",
            "TECHNICAL",
            "✓ VERIFIED",
        ] {
            assert!(out.contains(want), "{w}x{h}: {want} in {out}");
        }
    }
}

#[test]
fn c121_enter_on_coverage_opens_the_diff() {
    let (_dir, mut app) = evidence_from_overview(None);
    app.reduce(Action::Down);
    assert!(matches!(app.top(), Some(View::Evidence { cursor: 1, .. })));
    app.reduce(Action::Enter);
    assert_eq!(app.tab, Tab::Changes);
}

#[test]
fn c121_the_detail_follows_the_cursor_over_the_five_sections() {
    let (_dir, mut app) = evidence_from_overview(None);
    // INTEGRITY: Epochen und Signatur, die früheren Sektionen.
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("Epoch 1/1"), "{out}");
    assert!(out.contains("nobody vouches for them with a key"), "{out}");
    // COVERAGE: die Grenze und der Abgleich je Datei.
    app.reduce(Action::Down);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("Observation boundary"), "{out}");
    assert!(out.contains("◇ reported only       a"), "{out}");
    assert!(!out.contains("explained"), "{out}");
    // INTERPRETATION, LIMITS (die Liste bleibt), TECHNICAL.
    app.reduce(Action::Down);
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("shows the stored and the current interpretation"),
        "{out}"
    );
    app.reduce(Action::Down);
    let out = render_sized(&mut app, 160, 40);
    assert!(
        out.contains("At A1 observed, Minds does NOT prove:"),
        "{out}"
    );
    app.reduce(Action::Down);
    assert!(matches!(app.top(), Some(View::Evidence { cursor: 4, .. })));
    app.reduce(Action::Down);
    assert!(matches!(app.top(), Some(View::Evidence { cursor: 4, .. })));
}

/// Vor der Signaturprüfung ist der Record **sealed**, nicht intact: die
/// INTEGRITY-Zeile trägt den Trust-Glyph `◈` und das Wort, kein `✓` — wie
/// die Übersicht für denselben Zustand `… checking` zeigt, nie ✓ davor
/// (Design-Regel 2: ein Zustand, eine Darstellung). Erst das Urteil
/// VERIFIED macht daraus `✓ record intact`; TAMPERED bleibt `✗`.
#[test]
fn c121_integrity_is_sealed_not_intact_before_the_check() {
    let (_dir, mut app) = evidence_from_overview(None);
    let out = render_sized(&mut app, 160, 40);
    let row = |out: &str, label: &str| {
        out.lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("{label} row in {out}"))
            .to_string()
    };
    let integrity = row(&out, "INTEGRITY");
    assert!(
        integrity.contains("◈ record sealed · 1 seal, unsigned · chain closed (1 epoch) · A1"),
        "{integrity}"
    );
    assert!(!integrity.contains("✓"), "{integrity}");
    assert!(!out.contains("record intact"), "{out}");
    assert!(
        out.contains("This session's record is sealed and unsigned"),
        "{out}"
    );
    assert!(!out.contains("record is intact"), "{out}");

    let (_dir, mut app) = evidence_from_overview(Some(Ok(crate::VerifyVerdict::Verified)));
    let out = render_sized(&mut app, 160, 40);
    let integrity = row(&out, "INTEGRITY");
    assert!(
        integrity.contains("✓ record intact · 1 seal, unsigned · chain closed (1 epoch) · A1"),
        "{integrity}"
    );
    assert!(!integrity.contains("◈ record sealed"), "{integrity}");
    assert!(
        out.contains("This session's record is intact and unsigned"),
        "{out}"
    );

    // Ein Fehler der Prüfung ist kein Urteil: sealed, nicht intact.
    let (_dir, mut app) = evidence_from_overview(Some(Err("verify binary not found".into())));
    let out = render_sized(&mut app, 160, 40);
    assert!(row(&out, "INTEGRITY").contains("◈ record sealed"), "{out}");
    assert!(!out.contains("record intact"), "{out}");

    // Manipuliert: ✗, nie sealed, nie intact.
    let (_dir, mut app) = tampered_without_a_check();
    let out = render_sized(&mut app, 160, 40);
    assert!(row(&out, "INTEGRITY").contains("✗ TAMPERED"), "{out}");
    assert!(!out.contains("record sealed"), "{out}");
    assert!(!out.contains("record intact"), "{out}");
}
