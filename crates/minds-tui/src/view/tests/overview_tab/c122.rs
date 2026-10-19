//! C12.2 · Tabs in Workflow-Reihenfolge, der Verify-Tab geht in die
//! Übersicht auf: `1 Overview · 2 Changes · 3 Sessions · 4 Requirements`.
//! SCOPE, CONFORMANCE und die Assurance-Stufe stehen in EVIDENCE; `v`
//! startet die Prüfung in der Übersicht. Die Versprechen der
//! Verify-Tab-Tests (Urteilswörter der CLI, kein VERIFIED ohne Prüfung,
//! Parität `! VERIFIED, INCOMPLETE`, Drossel, Worker) ziehen hierher um —
//! mit ihren Kommentaren.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use minds_reader::assurance::{Assurance, IntentSignature, IntentState, SignerKind};
use minds_reader::changes::LineClasses;

use super::*;
use crate::{CommitVerify, SessionAssurance, VerifyVerdict};

/// Eine Quelle, die das signaturabhängige Urteil vorgibt und zählt, wie
/// oft es angefragt wurde.
struct Signers {
    answer: RefCell<Option<CommitVerify>>,
    asked: Cell<usize>,
}

impl crate::Source for Signers {
    fn load(&self) -> minds_reader::Result<Inspection> {
        unreachable!("the tests reload with an explicit inspection")
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        Some("1".into())
    }
    fn verify(&self, _commit: CommitId) -> Result<CommitVerify, String> {
        self.asked.set(self.asked.get() + 1);
        self.answer
            .borrow()
            .clone()
            .ok_or_else(|| "store unreadable".to_string())
    }
}

fn signers(answer: Option<CommitVerify>) -> Signers {
    Signers {
        answer: RefCell::new(answer),
        asked: Cell::new(0),
    }
}

/// Eine Prüfung, die Zeit braucht — und nur eigene Daten hält (Send).
struct Slow(CommitVerify);

impl crate::Background for Slow {
    fn verify(&mut self, _commit: CommitId) -> Result<CommitVerify, String> {
        std::thread::sleep(Duration::from_millis(50));
        Ok(self.0.clone())
    }
}

fn witnessed(tampered: bool) -> CommitVerify {
    verdict(
        tampered,
        if tampered {
            VerifyVerdict::Tampered
        } else {
            VerifyVerdict::Verified
        },
    )
}

/// Wie [`witnessed`], mit dem Urteil, das `minds verify` fällt — die
/// Session a des Fixtures, mit einer Datei außerhalb des Bereichs.
fn verdict(tampered: bool, verdict: VerifyVerdict) -> CommitVerify {
    CommitVerify {
        verdict: Ok(verdict),
        sessions: vec![SessionAssurance {
            session: sid('a'),
            level: Assurance::A2Witnessed,
            reason: Some("not reproduced in CI".into()),
            intent: IntentState::Bound {
                anchor_id: minds_core::ContentHash::from_bytes([1u8; 32]),
                chained: true,
                signature: IntentSignature::Valid(SignerKind::SoftwareKey),
                snapshot_matches: true,
                from_session_start: true,
                changed_mid_session: false,
            },
            tampered,
        }],
        out_of_scope: Some(vec!["Cargo.lock".into()]),
        out_of_scope_paths: vec!["Cargo.lock".into()],
        scope_note: None,
        conformance: None,
    }
}

/// Die Übersicht auf dem Agent-Commit (`feat(sort): agent work`).
fn on_agent<'r>(repo: &'r Repo, inspection: Inspection, commits: &[CommitId]) -> App<'r> {
    let mut app = opened(repo, inspection);
    assert!(app.overview.as_mut().unwrap().select_commit(commits[1]));
    app
}

fn text_at(app: &mut App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal
        .backend()
        .to_string()
        .lines()
        .map(|l| l.trim_matches('"').to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Die Zeile des Details mit `label`.
fn line_with(out: &str, label: &str) -> String {
    out.lines()
        .find(|l| l.contains(label))
        .unwrap_or_else(|| panic!("no {label} in\n{out}"))
        .to_string()
}

/// Ein belegter, signierter Anker mit Bereich — wie der Requirements-Tab
/// ihn liest.
fn proven_anchor() -> crate::IntentDetail {
    crate::IntentDetail {
        source: "file:docs/anforderung.md@abc".into(),
        content: minds_core::ContentHash::from_bytes([2; 32]),
        scope: vec!["src/**".into()],
        version: Some("HEAD version".into()),
        proof: Ok(()),
        signature: IntentSignature::Valid(SignerKind::SoftwareKey),
        snapshot_len: 12,
        snapshot: Some(vec!["Sort stably.".into()]),
        snapshot_clipped: false,
    }
}

fn all_observed(added: usize) -> LineClasses {
    LineClasses {
        added,
        explained: added,
        fs_only: 0,
        stated: 0,
        unexplained: 0,
        not_assessed: 0,
        skipped_files: 0,
        files: 1,
        reasons: Vec::new(),
    }
}

/// Alles oberhalb von SCOPE ✓: Urteil, Zeilen, Deutung, Anforderung.
fn everything_above_scope_checked(app: &mut App, commits: &[CommitId], cli: CommitVerify) {
    let state = app.overview.as_mut().unwrap();
    state.lines.insert(commits[1], Ok(all_observed(3)));
    state.requirements.insert(
        minds_core::ContentHash::from_bytes([1u8; 32]),
        Ok(proven_anchor()),
    );
    assert!(app.fill_overview_verify(&signers(Some(cli))));
}

// ---------------------------------------------------------------------
// C12.2-AC1 · Tabs in Workflow-Reihenfolge, kein Verify-Tab
// ---------------------------------------------------------------------

/// `1 Overview · 2 Changes · 3 Sessions · 4 Requirements` — F1–F4, `1`–`4`,
/// Tab/Shift-Tab; der Verify-Tab ist weg, der Intent-Tab heißt Requirements.
#[test]
fn c122_tabs_are_in_workflow_order_and_verify_is_gone() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    assert_eq!(
        Tab::ALL,
        [
            Tab::Overview,
            Tab::Changes,
            Tab::Sessions,
            Tab::Requirements
        ]
    );
    let out = text_at(&mut app, 160, 40);
    let head = out.lines().next().unwrap();
    for label in [
        "F1 Overview",
        "F2 Changes",
        "F3 Sessions",
        "F4 Requirements",
    ] {
        assert!(head.contains(label), "{head}");
    }
    assert!(!head.contains("Verify"), "{head}");
    assert!(!head.contains("Intent"), "{head}");
    assert!(!head.contains("F5"), "{head}");
    // Unter 100 Spalten die Kurzlabels, in derselben Reihenfolge.
    let out = text_at(&mut app, 80, 24);
    let head = out.lines().next().unwrap();
    for label in ["F1 Ovw", "F2 Chg", "F3 Ses", "F4 Req"] {
        assert!(head.contains(label), "{head}");
    }
    assert!(!head.contains("F3 Ver"), "{head}");
    // Die Ziffern folgen der Reihenfolge; eine fünfte gibt es nicht.
    app.reduce(Action::TabTo(1));
    assert_eq!(app.tab, Tab::Changes);
    app.reduce(Action::TabTo(2));
    assert_eq!(app.tab, Tab::Sessions);
    app.reduce(Action::TabTo(3));
    assert_eq!(app.tab, Tab::Requirements);
    app.reduce(Action::TabTo(4));
    assert_eq!(app.tab, Tab::Requirements, "no fifth tab");
    // Der Breadcrumb nennt den Tab Requirements.
    let out = text_at(&mut app, 160, 40);
    let crumb = out.lines().nth(1).unwrap();
    assert!(crumb.contains("Requirements"), "{crumb}");
    assert!(!crumb.contains("Intent"), "{crumb}");
    // Tab/Shift-Tab laufen im Kreis über die vier.
    app.reduce(Action::CycleTab(true));
    assert_eq!(app.tab, Tab::Overview);
    app.reduce(Action::CycleTab(false));
    assert_eq!(app.tab, Tab::Requirements);
    app.reduce(Action::CycleTab(false));
    assert_eq!(app.tab, Tab::Sessions);
}

/// Hilfe und Fußzeile kennen vier Tabs; `v` steht in der Übersicht.
#[test]
fn c122_the_help_and_the_footer_name_four_tabs() {
    let (_dir, repo, _, inspection) = history();
    let mut app = opened(&repo, inspection);
    app.reduce(Action::Help);
    let out = text_at(&mut app, 160, 40);
    assert!(out.contains("1 … 4"), "{out}");
    assert!(
        out.contains("Overview · Changes · Sessions · Requirements"),
        "{out}"
    );
    assert!(!out.contains("Verify:"), "{out}");
    assert!(!out.contains("Intent:"), "{out}");
    assert!(out.contains("Requirements:"), "{out}");
    app.reduce(Action::Back);
    let out = text_at(&mut app, 160, 40);
    let keys = out.lines().last().unwrap();
    assert!(keys.contains("v verify"), "{keys}");
}

// ---------------------------------------------------------------------
// C12.2-AC2 · Die Verify-Inhalte stehen in EVIDENCE
// ---------------------------------------------------------------------

/// Sofort da: Achsen aus dem Reader; die Signaturen werden noch geprüft —
/// und das steht da, statt zu raten. (Umzug von
/// `the_verdict_comes_first_and_says_what_is_still_being_checked`.)
#[test]
fn c122_before_the_verdict_integrity_and_scope_say_checking() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let out = text_at(&mut app, 160, 40);
    // Ohne Signaturprüfung kein VERIFIED — der Reader allein sieht keine
    // ungültige Witness-Signatur.
    assert!(line_with(&out, "INTEGRITY").contains("… checking"), "{out}");
    assert!(!out.contains("VERIFIED"), "{out}");
    let scope = line_with(&out, "SCOPE");
    assert!(scope.contains("… checking"), "{scope}");
    assert!(!scope.contains('✓'), "{scope}");
}

/// Mit den Signern: Urteil, Stufe und Scope aus der Quelle — einmal
/// geholt, nicht je Frame. (Umzug von
/// `the_signed_parts_come_from_the_source_once`.)
#[test]
fn c122_the_signed_parts_come_from_the_source_once() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let source = signers(Some(witnessed(false)));
    assert!(app.fill_overview_verify(&source));
    assert!(!app.fill_overview_verify(&source));
    assert_eq!(source.asked.get(), 1);
    let out = text_at(&mut app, 160, 40);
    let integrity = line_with(&out, "INTEGRITY");
    assert!(integrity.contains("✓ VERIFIED"), "{integrity}");
    // Die Assurance-Stufe als Zusatz in INTEGRITY.
    assert!(integrity.contains("· A2 witnessed"), "{integrity}");
    assert!(!out.contains("checking signatures"), "{out}");
    let scope = line_with(&out, "SCOPE");
    assert!(
        scope.contains("◦ 1 file outside the scope: Cargo.lock"),
        "{scope}"
    );
}

/// Alles im Bereich: ein ✓ mit den Globs des gelesenen Ankers — ohne
/// gelesenen Anker das Wort „declared scope".
#[test]
fn c122_scope_inside_says_so_with_the_globs() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = witnessed(false);
    cli.out_of_scope = Some(Vec::new());
    cli.out_of_scope_paths = Vec::new();
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    assert!(
        line_with(&out, "SCOPE").contains("✓ all changes inside the declared scope"),
        "{out}"
    );
    app.overview.as_mut().unwrap().requirements.insert(
        minds_core::ContentHash::from_bytes([1u8; 32]),
        Ok(proven_anchor()),
    );
    let out = text_at(&mut app, 160, 40);
    assert!(
        line_with(&out, "SCOPE").contains("✓ all changes inside src/**"),
        "{out}"
    );
    // Kein Bereich geprüft: `·` mit dem Grund — trifft nicht zu, kein Fehler.
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = witnessed(false);
    cli.out_of_scope = None;
    cli.out_of_scope_paths = Vec::new();
    cli.scope_note = Some("intent not bound".into());
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    assert!(
        line_with(&out, "SCOPE").contains("· not assessed (intent not bound)"),
        "{out}"
    );
}

/// Eine ungültige Witness-Signatur ist TAMPERED — ✗ in INTEGRITY. (Umzug
/// von `an_invalid_witness_signature_is_tampered`.)
#[test]
fn an_invalid_witness_signature_is_tampered() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(witnessed(true)))));
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("✗ TAMPERED"), "{out}");
}

/// Eine gescheiterte Prüfung ist nie VERIFIED und sagt, warum — auch der
/// Scope. (Umzug von `a_failed_check_is_never_verified_and_says_why`.)
#[test]
fn a_failed_check_is_never_verified_and_says_why() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(None)));
    let out = text_at(&mut app, 160, 40);
    assert!(
        line_with(&out, "INTEGRITY").contains("not checked — store unreadable"),
        "{out}"
    );
    assert!(!out.contains("VERIFIED"), "{out}");
    assert!(
        line_with(&out, "SCOPE").contains("· not checked (store unreadable)"),
        "{out}"
    );
}

/// Eine Session ohne Seal: nichts zu verifizieren — wie `verify`. (Umzug
/// von `a_session_without_a_seal_is_not_verifiable`.)
#[test]
fn a_session_without_a_seal_is_not_verifiable() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(verdict(false, VerifyVerdict::NotVerifiable)))));
    let out = text_at(&mut app, 160, 40);
    assert!(
        line_with(&out, "INTEGRITY").contains("? NOT VERIFIABLE"),
        "{out}"
    );
}

/// Kein ✓ „intact" neben NOT VERIFIABLE. (Umzug von
/// `not_verifiable_shows_no_intact_check`.)
#[test]
fn not_verifiable_shows_no_intact_check() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(verdict(false, VerifyVerdict::NotVerifiable)))));
    let out = text_at(&mut app, 160, 40);
    let integrity = line_with(&out, "INTEGRITY");
    assert!(integrity.contains("? NOT VERIFIABLE"), "{integrity}");
    assert!(!integrity.contains('✓'), "{integrity}");
    assert!(!out.contains("✓ intact"), "{out}");
}

/// Ein verdeckter Tab startet kein ssh-keygen; sichtbar wird geprüft.
/// (Umzug von `a_hidden_tab_does_not_check`.)
#[test]
fn a_hidden_tab_does_not_check() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    app.reduce(Action::TabTo(2));
    let source = signers(Some(witnessed(false)));
    assert!(!app.fill_overview_verify(&source));
    assert_eq!(source.asked.get(), 0);
    app.reduce(Action::TabTo(0));
    assert!(app.fill_overview_verify(&source));
    assert_eq!(source.asked.get(), 1);
}

/// Neu laden prüft die Signer immer neu: Eine ausgetauschte `seal.sig`
/// ändert keine Session-Id — ein übernommenes Urteil könnte VERIFIED
/// zeigen, wo `verify` TAMPERED sagt. (Umzug von
/// `reload_always_checks_the_signers_again`.)
#[test]
fn reload_always_checks_the_signers_again() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection.clone(), &commits);
    app.stamp = Some("1".into());
    assert!(app.fill_overview_verify(&signers(Some(witnessed(false)))));
    assert!(text_at(&mut app, 160, 40).contains("✓ VERIFIED"));
    let tampered = signers(Some(witnessed(true)));
    app.reload(Ok(inspection), Some("2".into()), now());
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("… checking"), "{out}");
    assert!(!out.contains("VERIFIED"), "{out}");
    assert!(app.fill_overview_verify(&tampered));
    assert_eq!(tampered.asked.get(), 1);
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("✗ TAMPERED"), "{out}");
}

/// Das Urteil ist das von `minds verify`: Sieht `verify` eine Lücke, die
/// der Reader nicht kennt (ein Seal nur im Namensraum), gilt INCOMPLETE.
/// (Umzug von `the_verdict_is_the_one_of_minds_verify`.)
#[test]
fn the_verdict_is_the_one_of_minds_verify() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(verdict(false, VerifyVerdict::Incomplete)))));
    let out = text_at(&mut app, 160, 40);
    let integrity = line_with(&out, "INTEGRITY");
    assert!(integrity.contains("! VERIFIED, INCOMPLETE"), "{integrity}");
    assert!(!integrity.contains("✓ VERIFIED"), "{integrity}");
}

/// Ohne geprüftes Urteil nie VERIFIED und nie INCOMPLETE — beides
/// behauptet intakte Integrität. (Umzug von
/// `without_a_checked_verdict_neither_verified_nor_incomplete`.)
#[test]
fn without_a_checked_verdict_neither_verified_nor_incomplete() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut failed = witnessed(false);
    failed.verdict = Err("minds verify failed".into());
    assert!(app.fill_overview_verify(&signers(Some(failed))));
    let out = text_at(&mut app, 160, 40);
    let integrity = line_with(&out, "INTEGRITY");
    assert!(integrity.contains("not checked"), "{integrity}");
    assert!(integrity.contains("minds verify failed"), "{integrity}");
    assert!(!out.contains("VERIFIED"), "{out}");
    assert!(!out.contains("INCOMPLETE"), "{out}");
    assert!(!integrity.contains('✓'), "{integrity}");
    assert!(!out.contains("✓ intact"), "{out}");
}

/// TAMPERED von `verify` selbst: kein ✓-Scope und keine Stufe aus einer
/// älteren Momentaufnahme daneben — `verify` zeigt dann beides nicht.
/// (Umzug von `a_tampered_verdict_shows_neither_scope_nor_level`.)
#[test]
fn a_tampered_verdict_shows_neither_scope_nor_level() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = verdict(false, VerifyVerdict::Tampered);
    cli.out_of_scope = Some(Vec::new());
    cli.out_of_scope_paths = Vec::new();
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("✗ TAMPERED"), "{out}");
    assert!(
        line_with(&out, "SCOPE").contains("· not assessed (integrity violated)"),
        "{out}"
    );
    assert!(!out.contains("✓ all changes"), "{out}");
    assert!(!out.contains("A2 witnessed"), "{out}");
}

/// Die Conformance steht je Regel in EVIDENCE — und ändert das Urteil
/// nicht (wie `verify` ohne `--conformance`). (Umzug von
/// `conformance_lists_each_rule_beside_the_verdict`.)
#[test]
fn conformance_lists_each_rule_beside_the_verdict() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = verdict(false, VerifyVerdict::Verified);
    cli.conformance = Some(sample_conformance(commits[1]));
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("✓ VERIFIED"), "{out}");
    let conformance = line_with(&out, "CONFORMANCE");
    assert!(conformance.contains("◦ 1 finding"), "{conformance}");
    assert!(
        conformance.contains("2 pass · 1 drift · 0 unknown"),
        "{conformance}"
    );
    assert!(
        out.contains("spec b3-090909090909 signed by human@example"),
        "{out}"
    );
    assert!(
        out.contains("✗ drift  app-not-db  app → core → db"),
        "{out}"
    );
    assert!(out.contains("expected app does not depend on db"), "{out}");
    // Bestandene Regeln stehen nicht einzeln — nur die Befunde.
    assert!(!out.contains("✓ pass    config"), "{out}");
}

/// Ohne Soll keine CONFORMANCE-Zeile — kein leerer Platz, kein PASS.
/// (Umzug von `without_a_spec_the_verify_tab_says_so`.)
#[test]
fn c122_without_a_spec_there_is_no_conformance_line() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(verdict(false, VerifyVerdict::Verified)))));
    let out = text_at(&mut app, 160, 40);
    assert!(!out.contains("CONFORMANCE"), "{out}");
    assert!(!out.contains("no spec"), "{out}");
}

/// Ein konformer Commit: `✓ conforms` mit der Quelle des Solls.
#[test]
fn c122_a_conforming_commit_says_so() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = verdict(false, VerifyVerdict::Verified);
    let mut report = sample_conformance(commits[1]);
    for rule in &mut report.rules {
        rule.outcome = minds_reader::conformance::Outcome::Pass;
    }
    cli.conformance = Some(report);
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    let conformance = line_with(&out, "CONFORMANCE");
    assert!(conformance.contains("✓ conforms"), "{conformance}");
    assert!(conformance.contains("3 pass"), "{conformance}");
    assert!(out.contains("signed by human@example"), "{out}");
    assert!(!out.contains("drift  "), "{out}");
}

// ---------------------------------------------------------------------
// C12.2-AC3 · NEXT kennt SCOPE und CONFORMANCE, nennt den Requirements-Tab
// ---------------------------------------------------------------------

/// Alles darüber ✓, eine Datei außerhalb des Bereichs: NEXT führt zum
/// Diff — und `d` öffnet ihn auf diesem Commit. (Umzug des Versprechens
/// von `enter_on_scope_opens_the_diff`.)
#[test]
fn c122_next_names_the_files_outside_the_scope_and_d_opens_the_diff() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    everything_above_scope_checked(&mut app, &commits, witnessed(false));
    let out = text_at(&mut app, 160, 40);
    let next = line_with(&out, "NEXT");
    assert!(
        next.contains("→ d: inspect the 1 file outside the scope"),
        "{next}"
    );
    assert_eq!(out.matches("NEXT").count(), 2, "detail and footer\n{out}");
    app.reduce(Action::Diff);
    assert_eq!(app.tab, Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commits[1]));
}

/// Scope sauber, ein Befund gegen das Soll: NEXT nennt ihn.
#[test]
fn c122_next_names_the_conformance_findings() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = witnessed(false);
    cli.out_of_scope = Some(Vec::new());
    cli.out_of_scope_paths = Vec::new();
    cli.conformance = Some(sample_conformance(commits[1]));
    everything_above_scope_checked(&mut app, &commits, cli);
    let out = text_at(&mut app, 160, 40);
    let next = line_with(&out, "NEXT");
    assert!(
        next.contains("→ d: inspect the diff — 1 finding against the signed spec"),
        "{next}"
    );
    assert_eq!(out.matches("NEXT").count(), 2, "detail and footer\n{out}");
}

/// Scope und Soll in Ordnung: NEXT geht weiter zum Review.
#[test]
fn c122_scope_and_conformance_in_order_fall_through_to_review() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = witnessed(false);
    cli.out_of_scope = Some(Vec::new());
    cli.out_of_scope_paths = Vec::new();
    everything_above_scope_checked(&mut app, &commits, cli);
    let out = text_at(&mut app, 160, 40);
    let next = line_with(&out, "NEXT");
    assert!(next.contains("minds review"), "{next}");
}

/// Der Tab heißt Requirements — auch in der nächsten Aktion.
#[test]
fn c122_next_says_requirements_tab() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut cli = witnessed(false);
    cli.sessions[0].intent = IntentState::Bound {
        anchor_id: minds_core::ContentHash::from_bytes([1u8; 32]),
        chained: false,
        signature: IntentSignature::Unsigned,
        snapshot_matches: true,
        from_session_start: true,
        changed_mid_session: false,
    };
    app.overview
        .as_mut()
        .unwrap()
        .lines
        .insert(commits[1], Ok(all_observed(3)));
    assert!(app.fill_overview_verify(&signers(Some(cli))));
    let out = text_at(&mut app, 160, 40);
    let next = line_with(&out, "NEXT");
    assert!(
        next.contains("→ i: Requirements tab — the binding is not proven"),
        "{next}"
    );
    assert!(!out.contains("Intent tab"), "{out}");
}

// ---------------------------------------------------------------------
// C12.2-AC4 · `v` startet die Prüfung in der Übersicht; Worker und Drossel
// ---------------------------------------------------------------------

/// `v` in der Übersicht: dieselbe Prüfung jetzt noch einmal — sofort, nicht
/// gedrosselt. (Umzug von `c5_v_starts_the_check_again`.)
#[test]
fn c5_v_starts_the_check_again() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    assert!(app.fill_overview_verify(&signers(Some(witnessed(false)))));
    assert!(text_at(&mut app, 160, 40).contains("✓ VERIFIED"));
    let state = app.overview.as_ref().unwrap();
    assert!(state.check_due().is_none(), "nothing pending");
    app.reduce(Action::Verify);
    assert_eq!(app.tab, Tab::Overview);
    let state = app.overview.as_ref().unwrap();
    assert_eq!(state.check_due(), Some(commits[1]), "the check runs again");
    assert!(state.urgent, "at once");
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("… checking"), "{out}");
    assert!(!out.contains("VERIFIED"), "{out}");
}

/// `v` anderswo öffnet die Übersicht auf dem Commit und prüft ihn; ohne
/// Commit in der gelesenen Historie ein Hinweis, nie still nichts (B4).
#[test]
fn c122_v_elsewhere_opens_the_overview_on_the_commit() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    app.reduce(Action::Diff);
    assert_eq!(app.tab, Tab::Changes);
    app.reduce(Action::Verify);
    assert_eq!(app.tab, Tab::Overview);
    let state = app.overview.as_ref().unwrap();
    assert!(matches!(
        state.selected(),
        Some(crate::overview::Selected::Commit(row)) if row.id == commits[1]
    ));
    assert_eq!(state.check_due(), Some(commits[1]));
    assert!(state.urgent);
    // Zurück über den Verlauf (B2).
    app.reduce(Action::Back);
    assert_eq!(app.tab, Tab::Changes);
    // Eine Session, deren Commit nicht in der gelesenen Historie liegt.
    let (_dir, repo) = super::repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Verify);
    assert_eq!(app.tab, Tab::Sessions);
    let out = text_at(&mut app, 160, 40);
    assert!(out.contains("minds verify"), "{out}");
}

/// Die Drossel: Vom Nutzer verlangt sofort, nach einem Neuladen erst
/// wieder nach `every` — und ohne ausstehende Prüfung nie. (Umzug von
/// `checks_after_a_reload_are_throttled_but_user_requests_are_not`.)
#[test]
fn checks_after_a_reload_are_throttled_but_user_requests_are_not() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection.clone(), &commits);
    let every = Duration::from_secs(5);
    let state = app.overview.as_ref().unwrap();
    assert!(state.check_due_in(Duration::ZERO, every), "opened: at once");
    assert!(app.fill_overview_verify(&signers(Some(witnessed(false)))));
    assert!(
        !app.overview.as_ref().unwrap().check_due_in(every, every),
        "nothing pending"
    );
    app.stamp = Some("1".into());
    app.reload(Ok(inspection.clone()), Some("2".into()), now());
    let state = app.overview.as_ref().unwrap();
    assert!(
        !state.check_due_in(Duration::from_secs(1), every),
        "after a reload: throttled"
    );
    assert!(state.check_due_in(every, every));
    // `r` (voll) prüft sofort.
    app.full_next = true;
    app.reload(Ok(inspection), Some("3".into()), now());
    assert!(
        app.overview
            .as_ref()
            .unwrap()
            .check_due_in(Duration::ZERO, every)
    );
}

/// Zurück im Tab mit ausstehender Prüfung: sofort prüfen, nicht drosseln.
/// (Umzug von `returning_to_the_tab_checks_a_pending_reload_at_once`.)
#[test]
fn returning_to_the_tab_checks_a_pending_reload_at_once() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection.clone(), &commits);
    let every = Duration::from_secs(5);
    assert!(app.fill_overview_verify(&signers(Some(witnessed(false)))));
    app.reduce(Action::TabTo(2));
    app.stamp = Some("1".into());
    app.reload(Ok(inspection), Some("2".into()), now());
    app.reduce(Action::TabTo(0));
    let state = app.overview.as_ref().unwrap();
    assert_eq!(state.check_due(), Some(commits[1]));
    assert!(state.check_due_in(Duration::ZERO, every));
}

/// B6: ein Worker-Thread prüft, das Ergebnis kommt über einen Kanal und
/// wird beim nächsten Tick übernommen — die Oberfläche reagiert derweil.
/// (Umzug von `b6_the_check_runs_in_a_worker_and_the_ui_stays_responsive`.)
#[test]
fn b6_the_check_runs_in_a_worker_and_the_ui_stays_responsive() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut worker = crate::app::Worker::spawn(Box::new(Slow(witnessed(false))));
    assert!(
        app.send_overview_check(&mut worker),
        "the shown commit goes to the worker"
    );
    // Während die Prüfung läuft: „checking", und die Oberfläche reagiert.
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "INTEGRITY").contains("… checking"), "{out}");
    assert!(!app.send_overview_check(&mut worker), "one check at a time");
    app.reduce(Action::Down);
    // Der nächste Tick übernimmt das Ergebnis — für den Commit, nach dem
    // gefragt wurde.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.take_checks(&mut worker) {
        assert!(Instant::now() < deadline, "the worker answers");
        std::thread::sleep(Duration::from_millis(5));
    }
    let state = app.overview.as_ref().unwrap();
    assert!(state.checks.contains_key(&commits[1]));
    assert!(state.asked.is_none());
}

/// B6: „Das bestehende Drosseln (`VERIFY_EVERY`) bleibt" — auch mit Worker:
/// Ein Neuladen stößt eine Prüfung erst nach `VERIFY_EVERY` an, ein vom
/// Nutzer verlangter Check sofort; der Worker nimmt nur eine zur Zeit.
/// (Umzug von `b6_the_throttle_stays_with_the_worker`.)
#[test]
fn b6_the_throttle_stays_with_the_worker() {
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let every = crate::app::VERIFY_EVERY;
    let state = app.overview.as_mut().unwrap();
    state.urgent = false;
    assert!(
        !state.check_due_in(every / 2, every),
        "throttled after a reload"
    );
    assert!(
        state.check_due_in(every, every),
        "due once VERIFY_EVERY has passed"
    );
    state.urgent = true;
    assert!(
        state.check_due_in(Duration::ZERO, every),
        "a user request is not throttled"
    );
    let mut worker = crate::app::Worker::spawn(Box::new(Slow(witnessed(false))));
    assert!(app.send_overview_check(&mut worker));
    assert!(
        !app.overview.as_ref().unwrap().urgent,
        "the request is consumed"
    );
    assert!(!app.send_overview_check(&mut worker), "one check at a time");
}

/// Ein ASCII-Spinner — er dreht sich je Tick, im EVIDENCE-Kopf, solange der
/// Worker prüft. (Umzug von `b6_the_spinner_is_ascii_and_turns`.)
#[test]
fn b6_the_spinner_is_ascii_and_turns() {
    let frames: Vec<char> = (0..5).map(crate::app::spinner).collect();
    assert_eq!(frames, ['|', '/', '-', '\\', '|']);
    let (_dir, repo, commits, inspection) = history();
    let mut app = on_agent(&repo, inspection, &commits);
    let mut worker = crate::app::Worker::spawn(Box::new(Slow(witnessed(false))));
    assert!(app.send_overview_check(&mut worker));
    app.spin = 1;
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "EVIDENCE").contains("checking /"), "{out}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.take_checks(&mut worker) {
        assert!(Instant::now() < deadline, "the worker answers");
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = text_at(&mut app, 160, 40);
    assert!(line_with(&out, "EVIDENCE").contains("checked "), "{out}");
}
