//! C9.4 · Ein Wort je Evidenz-Zustand, tabübergreifend
//! (docs/spikes/auftrag-batch-2.md, Paket 6).

use minds_reader::changes::LineClasses;

use super::*;
use crate::theme;

fn text_of(lines: &[ratatui::text::Line<'_>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Die vier Zustandswörter stehen im Register, und die Hilfe-Legende
/// entsteht daraus: `● observed · ◍ fs only · ◇ reported · ◦ not observed`.
#[test]
fn c94_the_state_words_live_in_the_register_and_the_legend() {
    let meaning = |glyph: &str| {
        theme::REGISTER
            .iter()
            .find(|e| e.symbol == glyph)
            .map(|e| e.meaning)
            .unwrap()
    };
    assert_eq!(meaning("●"), "observed");
    assert_eq!(meaning("◍"), "fs only");
    assert_eq!(meaning("◇"), "reported");
    assert_eq!(meaning("◦"), "not observed");
    for (class, glyph, word) in [
        (ReconClass::Explained, "●", "observed"),
        (ReconClass::ExplainedFsOnly, "◍", "fs only"),
        (ReconClass::ReportedOnly, "◇", "reported"),
        (ReconClass::Unexplained, "◦", "not observed"),
    ] {
        let (g, w, _) = theme::state(class);
        assert_eq!((g, w), (glyph, word), "{class:?}");
        assert_eq!(meaning(glyph), word, "{class:?}: register and state agree");
    }
    let legend = crate::view::help::legend(120).join("\n");
    for item in ["● observed", "◍ fs only", "◇ reported", "◦ not observed"] {
        assert!(legend.contains(item), "{item} missing:\n{legend}");
    }
    assert!(!legend.contains("stated"), "{legend}");
    assert!(!legend.contains("◦ open"), "{legend}");
}

/// Overview COVERAGE: `◇ 95 of 95 added lines reported, 0 observed`
/// statt „stated"; die Zeile `· 95 added lines` entfällt; der Grund-Satz
/// einer unerklärten Zeile bleibt. (Klasse 2, Review Batch 5: `reported,
/// not observed` → `reported, 0 observed` und `unexplained` → `not
/// observed` — ein Wortlaut mit der Evidence-Ebene, C9.4.)
#[test]
fn c94_overview_coverage_says_reported_not_observed() {
    let text = |c: LineClasses| text_of(&crate::view::overview::coverage_lines(Some(&Ok(c))));
    let reported = text(LineClasses {
        added: 95,
        stated: 95,
        ..LineClasses::default()
    });
    assert!(
        reported.contains("◇ 95 of 95 added lines reported, 0 observed"),
        "{reported}"
    );
    assert!(!reported.contains("reported, not observed"), "{reported}");
    assert!(!reported.contains("stated"), "{reported}");
    assert!(!reported.contains("· 95 added lines"), "{reported}");
    assert!(!reported.contains('✓'), "{reported}");

    let mixed = text(LineClasses {
        added: 10,
        stated: 4,
        unexplained: 6,
        files: 1,
        reasons: vec![minds_reader::changes::ReasonCount {
            reason: "a tool claim writes the file without a hash",
            lines: 6,
        }],
        ..LineClasses::default()
    });
    assert!(
        mixed.contains("◦ 6 of 10 added lines not observed · 4 reported"),
        "{mixed}"
    );
    assert!(!mixed.contains("unexplained"), "{mixed}");
    assert!(!mixed.contains("stated"), "{mixed}");
    assert!(
        mixed.contains("— a tool claim writes the file without a hash: all 6"),
        "the reason sentence stays:\n{mixed}"
    );

    let fs_only = text(LineClasses {
        added: 3,
        fs_only: 3,
        ..LineClasses::default()
    });
    assert!(
        fs_only.contains("◍ 3 of 3 added lines fs only"),
        "{fs_only}"
    );
    assert!(!fs_only.contains("· 3 added lines"), "{fs_only}");
}

/// Overview-Detail, FILES-Titel und Diff-Legende rechnen mit einer
/// Reader-Funktion: dieselbe Fixture liefert an allen drei Stellen dieselben
/// Zahlen.
#[test]
fn c94_the_numbers_come_from_one_reader_function() {
    let (_dir, repo) = repo();
    let set = set();
    let total = set.line_classes();
    let per_file: Vec<LineClasses> = set.files.iter().map(|f| f.line_classes()).collect();
    let sum = |pick: fn(&LineClasses) -> usize| per_file.iter().map(pick).sum::<usize>();
    assert_eq!(total.added, sum(|c| c.added));
    assert_eq!(total.explained, sum(|c| c.explained));
    assert_eq!(total.fs_only, sum(|c| c.fs_only));
    assert_eq!(total.stated, sum(|c| c.stated));
    assert_eq!(total.unexplained, sum(|c| c.unexplained));
    assert_eq!(total.files, per_file.len());

    let mut app = app_with_changes(&repo);
    let out = render(&mut app);
    // FILES-Titel.
    assert!(
        out.contains(&format!(
            "FILES · {} files · +{} −{}",
            total.files,
            total.added,
            set.removed()
        )),
        "{out}"
    );
    // Diff-Legende der gewählten Datei.
    let first = &per_file[0];
    for part in [
        format!("● observed {}", first.explained),
        format!("◍ fs only {}", first.fs_only),
        format!("◇ reported {}", first.stated),
        format!("◦ not observed {}", first.unexplained),
    ] {
        assert!(out.contains(&part), "{part} missing:\n{out}");
    }
    // Overview COVERAGE.
    let coverage = text_of(&crate::view::overview::coverage_lines(Some(&Ok(
        total.clone()
    ))));
    // Klasse 2 (Review Batch 5): `unexplained` → `not observed`, die
    // berichteten Zeilen stehen im selben Satz (ein Wortlaut, C9.4).
    assert!(
        coverage.contains(&format!(
            "◦ {} of {} added lines not observed · {} reported",
            total.unexplained, total.added, total.stated
        )),
        "{coverage}"
    );
    assert!(!coverage.contains("unexplained"), "{coverage}");
}

/// Das Glossar kennt `reported`, nicht mehr `stated`; `not observed` bleibt.
#[test]
fn c94_the_glossary_says_reported() {
    assert!(crate::copy::sentence("reported").is_some());
    assert!(crate::copy::sentence("stated").is_none());
    assert!(crate::copy::sentence("not observed").is_some());
}
