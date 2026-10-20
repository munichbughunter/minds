//! Die Beobachtungen des Witness aus dem Urteil von `minds verify` erreichen
//! den Changes-Tab und die Zeilenzählung der Übersicht: Was der Witness mit
//! demselben Hash sah, ist `● observed`, nicht `◇ reported` — und die
//! Legende nennt dann kein `reported`. Vor dem Urteil (und ohne Witness)
//! bleibt jede Zeile berichtet: Der Index allein kennt keine Signer.

use minds_reader::reconcile::{FsObservation, ObservedAt};

use super::*;

/// Die abgeglichene Fixture (die Session schreibt `a` und `b`) in der
/// Übersicht; `head` ist ihr Commit.
fn overview() -> (tempfile::TempDir, App<'static>, CommitId) {
    let (dir, repo, inspection) = reconciled(false);
    let repo: &'static Repo = Box::leak(Box::new(repo));
    let mut app = App::new(inspection, repo, None);
    app.open_overview();
    let head = app.inspection.index().commits_of(sid('a'))[0];
    (dir, app, head)
}

/// Ein Urteil, dessen Beobachtungen beide Dateien mit genau dem
/// gemeldeten Inhalt zeigen.
fn witnessed() -> crate::CommitVerify {
    let observations = [("a", "one\ntwo\nthree\n"), ("b", "three\n")]
        .into_iter()
        .enumerate()
        .map(|(seq, (path, text))| FsObservation {
            path: path.into(),
            observed: ObservedAt {
                hash: Some(minds_core::ContentHash::from_bytes(
                    *blake3::hash(text.as_bytes()).as_bytes(),
                )),
                seq: seq as u64,
                at: Some("2026-10-02T10:00:05Z".into()),
            },
            content: None,
            opaque: false,
        })
        .collect();
    crate::CommitVerify {
        verdict: Ok(crate::VerifyVerdict::Verified),
        sessions: Vec::new(),
        out_of_scope: None,
        out_of_scope_paths: Vec::new(),
        scope_note: None,
        conformance: None,
        observations,
    }
}

/// Eine Quelle, deren Prüfung das bezeugte Urteil liefert.
struct Witness;

impl crate::Source for Witness {
    fn load(&self) -> minds_reader::Result<Inspection> {
        unreachable!("the test never reloads")
    }
    fn stamp(&self) -> Option<crate::Stamp> {
        Some("1".into())
    }
    fn verify(&self, _commit: CommitId) -> Result<crate::CommitVerify, String> {
        Ok(witnessed())
    }
}

/// Die Legende des Diffs zur gewählten Datei (`● observed n …`).
fn legend(out: &str) -> String {
    out.lines()
        .find(|l| l.contains("● observed"))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no legend:\n{out}"))
}

#[test]
fn the_verdicts_observations_make_reported_lines_observed() {
    let (_dir, mut app, head) = overview();
    // Ohne Urteil: berichtet, nicht beobachtet.
    app.open_changes(Some(head));
    let out = render(&mut app);
    let before = legend(&out);
    assert!(before.contains("◇ reported 3"), "{before}");
    assert!(before.contains("● observed 0"), "{before}");
    assert!(out.contains("◇ REPORTED"), "{out}");

    // Das Urteil kommt in der Übersicht an — der Changes-Tab zeigt diesen
    // Commit und wird an derselben Stelle neu gerechnet.
    app.tab = Tab::Overview;
    assert!(app.fill_overview_verify(&Witness));
    app.tab = Tab::Changes;
    let out = render(&mut app);
    let after = legend(&out);
    assert!(after.contains("● observed 3"), "{after}");
    assert!(after.contains("◇ reported 0"), "{after}");
    // „Warum diese Zeile?" sagt es: beobachtet, nicht berichtet — und die
    // Fußzeile zählt nichts Unbeobachtetes.
    assert!(out.contains("● OBSERVED"), "{out}");
    assert!(!out.contains("◇ REPORTED"), "{out}");
    assert!(!out.lines().last().unwrap().contains("n next"), "{out}");
    let set = app.changes.as_ref().unwrap().set.as_ref().unwrap();
    let classes: Vec<(String, Option<ReconClass>)> = set
        .files
        .iter()
        .map(|f| (f.path.clone(), f.class))
        .collect();
    assert_eq!(
        classes,
        [
            ("a".to_string(), Some(ReconClass::Explained)),
            ("b".to_string(), Some(ReconClass::Explained)),
        ]
    );
    let total = set.line_classes();
    assert_eq!((total.added, total.explained, total.stated), (4, 4, 0));

    // Die Übersicht zählt mit denselben Beobachtungen, sobald ihr Urteil da
    // ist: keine berichteten Zeilen mehr, alle beobachtet.
    let lines = app.overview.as_ref().unwrap().lines.get(&head).cloned();
    assert!(lines.is_none(), "die alte Zählung ist vergessen");
    app.tab = Tab::Overview;
    while app.fill_overview(&Witness) {}
    let counted = app
        .overview
        .as_ref()
        .unwrap()
        .lines
        .get(&head)
        .cloned()
        .expect("counted")
        .expect("assessed");
    assert_eq!(
        (counted.added, counted.explained, counted.stated),
        (4, 4, 0)
    );
}

/// Ein Changes-Tab, der nach dem Urteil geöffnet wird, rechnet von Anfang
/// an damit.
#[test]
fn a_changes_tab_opened_after_the_verdict_counts_observed_lines() {
    let (_dir, mut app, head) = overview();
    assert!(app.fill_overview_verify(&Witness));
    app.open_changes(Some(head));
    let out = render(&mut app);
    assert!(legend(&out).contains("● observed 3"), "{out}");
    assert!(out.contains("● OBSERVED"), "{out}");
    assert!(!out.contains("◇ REPORTED"), "{out}");
}
