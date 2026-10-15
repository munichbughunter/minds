//! Render-Proben der UX-Überarbeitung (docs/specs/tui-ux-auftrag.md) — je
//! Paket ein Abschnitt, jede Probe in den drei Referenzgrößen 80×24,
//! 120×30 und 160×40.

use super::*;
use crate::theme;
use ratatui::style::Color;

/// Die drei Referenzgrößen der Test-Konvention.
const SIZES: [(u16, u16); 3] = [(80, 24), (120, 30), (160, 40)];

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

fn buffer_sized(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::draw(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Die Vordergrundfarben aller Zellen, die `needle` in einer Zeile tragen.
fn colors_of(buffer: &ratatui::buffer::Buffer, needle: &str) -> Vec<Color> {
    let area = buffer.area;
    let mut found = Vec::new();
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(byte) = line.find(needle) {
            let start = line[..byte].chars().count() as u16;
            for x in start..start + needle.chars().count() as u16 {
                found.push(buffer[(x, y)].fg);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------
// A3 · Verworfene Ansätze und Constraints sichtbar machen
// ---------------------------------------------------------------------

const DISCARDED: [&str; 3] = [
    "Retry inside the HTTP client",
    "Circuit breaker via tower",
    "Fixed 1 s sleep between tries",
];

/// Eine Session mit mehrzeiligem Auftrag, zwei Constraints und drei
/// verworfenen Ansätzen — sonst wie `filled()`.
fn deliberate() -> Inspection {
    let mut s = session(
        "Fix retry handling\nOnly retry idempotent requests\nKeep the public API",
        "2026-07-25T14:10:00Z",
    );
    s.intent.constraints = vec!["no new dependency".into(), "max 3 retries".into()];
    s.intent.discarded = DISCARDED.iter().map(|d| d.to_string()).collect();
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    Inspection::from_index(
        Index::from_parts(sessions, commits),
        Vec::new(),
        "payment-service",
    )
}

#[test]
fn a3_why_lists_every_discarded_approach_as_a_claim() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(deliberate(), &repo, None);
        app.reduce(Action::Why);
        // Auf das INTENT-Glied: Session, Agent, Intent.
        app.reduce(Action::Down);
        app.reduce(Action::Down);
        let out = render_sized(&mut app, w, h);
        assert!(out.contains("Considered & discarded"), "{w}x{h}\n{out}");
        assert!(out.contains("Constraints"), "{w}x{h}\n{out}");
        assert!(out.contains("no new dependency"), "{w}x{h}\n{out}");
        for d in DISCARDED {
            // Bei 80 Spalten darf die Zeile mit … gekappt sein, der Anfang
            // muss lesbar bleiben.
            let head: String = d.chars().take(18).collect();
            assert!(out.contains(&head), "{w}x{h} {d}\n{out}");
        }
        assert!(!out.contains("Discarded: "), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        for color in colors_of(&buffer, "Circuit breaker") {
            assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        }
    }
}

#[test]
fn a3_graph_detail_shows_all_lines_of_the_intent() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(deliberate(), &repo, None);
        app.reduce(Action::Enter);
        let out = render_sized(&mut app, w, h);
        for d in DISCARDED {
            let head: String = d.chars().take(18).collect();
            assert!(out.contains(&head), "{w}x{h} {d}\n{out}");
        }
        assert!(out.contains("max 3 retries"), "{w}x{h}\n{out}");
        assert!(out.contains("Keep the public API"), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        for color in colors_of(&buffer, "Circuit breaker") {
            assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        }
    }
}

#[test]
fn a3_the_you_box_shows_the_request_and_counts_the_claims() {
    let (_dir, repo) = repo();
    let mut app = App::new(deliberate(), &repo, None);
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("Only retry idempotent requests"), "{out}");
    assert!(
        out.contains("◌ 3 discarded · 2 constraints — select YOU for details"),
        "{out}"
    );
}

#[test]
fn a3_empty_is_empty() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("Considered & discarded"), "{out}");
    assert!(!out.contains("Discarded: 0"), "{out}");
    app.reduce(Action::Back);
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains("discarded ·"), "{out}");
}

// ---------------------------------------------------------------------
// A2 · Claim und Evidence in der Why-Kette trennen
// ---------------------------------------------------------------------

/// Die Zelle des Glied-Markers vor `word` (`"INTENT"` …) — Symbol und Farbe.
fn marker_of(buffer: &ratatui::buffer::Buffer, word: &str) -> Option<(String, Color)> {
    let area = buffer.area;
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        let chars: Vec<char> = line.chars().collect();
        let text: String = chars.iter().collect();
        if let Some(byte) = text.find(&format!(" {word}")) {
            let at = text[..byte].chars().count();
            // Ein Glied-Kopf: Marker, Leerzeichen, Wort — am Zeilenanfang
            // der Kette (Rahmen und Liste davor sind erlaubt).
            if at >= 1 && !chars[at - 1].is_whitespace() {
                let x = (at - 1) as u16;
                return Some((buffer[(x, y)].symbol().to_string(), buffer[(x, y)].fg));
            }
        }
    }
    None
}

#[test]
fn a2_the_intent_link_is_a_claim_never_a_check() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        app.reduce(Action::Why); // Session a: Trailer, belegt
        let out = render_sized(&mut app, w, h);
        assert!(out.contains("◌ INTENT"), "{w}x{h}\n{out}");
        assert!(!out.contains("✓ INTENT"), "{w}x{h}\n{out}");
        let buffer = buffer_sized(&mut app, w, h);
        let (glyph, color) = marker_of(&buffer, "INTENT").expect("INTENT link");
        assert_eq!(glyph, "◌", "{w}x{h}");
        assert_ne!(color, theme::OK, "{w}x{h}: a claim is never OK-green");
        assert_eq!(color, theme::claim().2.fg.unwrap(), "{w}x{h}");
        // Ein belegtes Glied bleibt ✓ in OK.
        let (glyph, color) = marker_of(&buffer, "EVIDENCE").expect("EVIDENCE link");
        assert_eq!((glyph.as_str(), color), ("✓", theme::OK), "{w}x{h}");
    }
}

#[test]
fn a2_no_gap_never_includes_the_claims() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("✓ no gap in the evidence links"), "{out}");
    assert!(out.contains("intent is a recorded claim"), "{out}");
    assert!(!out.contains("every link is attested"), "{out}");
    assert!(!out.contains("Every link is attested"), "{out}");
}

#[test]
fn a2_an_unsigned_review_note_reads_as_a_claim() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Why);
    let out = render_sized(&mut app, 160, 40);
    let note = out
        .lines()
        .find(|l| l.contains("Retry bei 5xx fehlt"))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(note.contains("◌ pd"), "{out}");
    assert!(!note.contains("(signed)"), "{out}");
    // Ein Urteil nur aus unsignierten Notizen ist aufgezeichnet, nicht
    // belegt: Der REVIEW-Kopf trägt den Claim-Marker, nie das grüne ✓.
    assert!(out.contains("◌ REVIEW"), "{out}");
    assert!(!out.contains("✓ REVIEW"), "{out}");
    let buffer = buffer_sized(&mut app, 160, 40);
    let (glyph, color) = marker_of(&buffer, "REVIEW").expect("REVIEW link");
    assert_eq!(glyph, "◌");
    assert_ne!(color, theme::OK);
}

// ---------------------------------------------------------------------
// A1 · Glyph-Register: eine Bedeutung pro Glyph
// ---------------------------------------------------------------------

/// Jedes Glyph, das eine Theme-Funktion zeichnet, steht im Register — mit
/// genau der Bedeutung und Klasse, die der Aufruf meint. Weil das Register
/// je Glyph nur **einen** Eintrag hat, folgt: Jedes epistemische Glyph hat
/// genau eine Bedeutung, und kein strukturelles Element benutzt eines.
/// Erweitert `every_epistemic_state_differs_in_glyph_or_word_never_only_in_color`
/// (das bleibt unverändert und prüft weiter Glyph-oder-Wort).
#[test]
fn a1_every_epistemic_glyph_has_exactly_one_meaning() {
    use minds_core::{EvidenceMark as M, EvidenceSource as S, EvidenceStatus as St};
    use minds_reader::graph::{NodeKind, ToolKind};
    use minds_reader::model::{EvidenceState, EvidenceVerdict, Provenance, Verdict};
    use minds_reader::reconcile::ReconClass;
    use theme::GlyphClass::{Epistemic, Structural};

    let mut symbols = std::collections::BTreeSet::new();
    for entry in theme::REGISTER {
        assert!(symbols.insert(entry.symbol), "twice: {}", entry.symbol);
        assert!(!entry.meaning.is_empty() && !entry.word.is_empty());
        assert_eq!(
            entry.symbol.chars().count(),
            1,
            "one cell: {}",
            entry.symbol
        );
    }

    let mut uses: Vec<(String, theme::GlyphClass, &str, String)> = Vec::new();
    let mut add = |glyph: &str, class, meaning, at: String| {
        uses.push((glyph.to_string(), class, meaning, at));
    };
    for (source, meaning) in [
        (S::Observed, "observed"),
        (S::ContentDerived, "content"),
        (S::HumanDeclared, "stated"),
        (S::Heuristic, "inferred"),
    ] {
        for (status, check) in [
            (St::Verified, "passed"),
            (St::Partial, "partial"),
            (St::Unknown, "unchecked"),
            (St::Missing, "failed"),
        ] {
            let (g, _, _) = theme::evidence(Some(M { source, status }));
            let mut parts = g.split(' ');
            add(
                parts.next().unwrap(),
                Epistemic,
                meaning,
                format!("evidence {g}"),
            );
            add(
                parts.next().unwrap(),
                Epistemic,
                check,
                format!("evidence {g}"),
            );
        }
    }
    add(
        theme::evidence(None).0.as_str(),
        Epistemic,
        "none",
        "unlinked".into(),
    );
    let chained = |verdict| {
        Provenance::Chained(EvidenceState {
            verdict,
            seals: 1,
            events: 1,
            gaps: 0,
            pre_chain: 0,
            rejected: false,
            chain_closed: true,
            signed: 0,
        })
    };
    for (p, meaning) in [
        (chained(EvidenceVerdict::Verified), "sealed"),
        (chained(EvidenceVerdict::Incomplete), "incomplete"),
        (chained(EvidenceVerdict::Tampered), "failed"),
        (Provenance::Legacy, "none"),
    ] {
        add(
            theme::provenance(&p).0,
            Epistemic,
            meaning,
            "provenance".into(),
        );
    }
    add(theme::tampered().0, Epistemic, "failed", "tampered".into());
    add(theme::claim().0, Epistemic, "claim", "claim".into());
    add(
        theme::not_observed().0,
        Epistemic,
        "not observed",
        "not_observed".into(),
    );
    for (class, meaning) in [
        (ReconClass::Explained, "observed"),
        (ReconClass::ExplainedFsOnly, "fs only"),
        (ReconClass::ReportedOnly, "stated"),
        (ReconClass::Unexplained, "not observed"),
    ] {
        add(
            theme::recon(class).0,
            Epistemic,
            meaning,
            format!("{class:?}"),
        );
    }
    for (v, meaning) in [
        (Verdict::Open, "gap"),
        (Verdict::Approved, "approved"),
        (Verdict::NeedsWork, "needs work"),
        (Verdict::Rejected, "rejected"),
    ] {
        add(theme::verdict(v).0, Epistemic, meaning, format!("{v:?}"));
    }
    for (kind, class, meaning) in [
        (ToolKind::Read, Structural, "read"),
        (ToolKind::Edit, Structural, "edit"),
        (ToolKind::Exec, Structural, "exec"),
        (ToolKind::Delete, Structural, "delete"),
        (ToolKind::Other, Structural, "tool"),
        (ToolKind::Uninterpreted, Epistemic, "uninterpreted"),
    ] {
        add(theme::tool(kind).0, class, meaning, format!("{kind:?}"));
    }
    let change: minds_core::ChangeId = format!("I{}", "c".repeat(40)).parse().unwrap();
    for (kind, class, meaning) in [
        (NodeKind::Intent, Epistemic, "claim"),
        (NodeKind::Agent, Structural, "agent"),
        (
            NodeKind::Turn(minds_core::Role::Assistant),
            Structural,
            "turn",
        ),
        (NodeKind::Subagent(sid('f')), Structural, "agent"),
        (
            NodeKind::Handover {
                other: sid('e'),
                incoming: true,
            },
            Structural,
            "handover",
        ),
        (NodeKind::Change(change), Structural, "change"),
        (NodeKind::Commit(commit('1')), Structural, "change"),
        (NodeKind::Review(Verdict::Rejected), Epistemic, "rejected"),
    ] {
        add(theme::node(&kind).0, class, meaning, format!("{kind:?}"));
    }

    for (glyph, class, meaning, at) in uses {
        let entry = theme::REGISTER
            .iter()
            .find(|e| e.symbol == glyph)
            .unwrap_or_else(|| panic!("{at}: {glyph:?} is not in the register"));
        assert_eq!(entry.class, class, "{at}: {glyph} has the wrong class");
        assert_eq!(
            entry.meaning, meaning,
            "{at}: {glyph} means {:?}",
            entry.meaning
        );
    }
}

/// Die Legende der Hilfe kommt aus dem Register: jedes Glyph mit seinem Wort.
#[test]
fn a1_the_help_legend_is_generated_from_the_register() {
    let legend = super::super::help::legend(80).join("\n");
    for entry in theme::REGISTER {
        assert!(
            legend.contains(&format!("{} {}", entry.symbol, entry.word)),
            "{} {} missing:\n{legend}",
            entry.symbol,
            entry.word
        );
    }
    for line in super::super::help::legend(80) {
        assert!(line.chars().count() <= 80, "{line}");
    }
}

/// Der Listen-Bullet ist gestrichen — er trug keine Information.
#[test]
fn a1_the_session_list_has_no_bullet() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        let out = render_sized(&mut app, w, h);
        assert!(!out.contains("● Fix retry"), "{w}x{h}\n{out}");
        assert!(!out.contains("● Add exponential"), "{w}x{h}\n{out}");
        assert!(out.contains("Fix retry"), "{w}x{h}\n{out}");
    }
}

/// Die Hilfe zeigt die ganze Legende, wo Platz ist — die erzeugten Zeilen
/// dürfen nicht unter den Rand des Overlays fallen.
#[test]
fn a1_the_help_overlay_shows_the_whole_legend() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Help);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("State:"), "{out}");
    assert!(out.contains("Kinds:"), "{out}");
    assert!(out.contains("■ change / commit"), "{out}");
    assert!(out.contains("◇ stated (declared / reported only)"), "{out}");
}

// ---------------------------------------------------------------------
// A4 · Eine Komponente für den Vertrauenszustand („Trust Strip“)
// ---------------------------------------------------------------------

fn all_axes() -> Vec<minds_reader::model::TrustAxes> {
    use minds_reader::model::{CoverageAxis as C, IntegrityAxis as I, InterpretationAxis as D};
    let mut out = Vec::new();
    for integrity in [I::Sealed, I::Tampered, I::Legacy] {
        for coverage in [C::Complete, C::Incomplete, C::NotAssessable, C::NotRecorded] {
            for interpretation in [D::Interpreted, D::Uninterpreted(2)] {
                out.push(minds_reader::model::TrustAxes {
                    integrity,
                    coverage,
                    interpretation,
                });
            }
        }
    }
    out
}

/// Jede Kombination der drei Achsen ist in jeder Dichte unterscheidbar —
/// in `Glyphs` über die Glyphen, in den übrigen auch ohne Glyphen über die
/// Wörter. Feste Reihenfolge, drei Slots, nie verschmolzen.
#[test]
fn a4_every_axis_combination_differs_in_every_density() {
    use super::super::trust::{self, Density};
    let glyphs: Vec<char> = theme::REGISTER
        .iter()
        .flat_map(|e| e.symbol.chars())
        .collect();
    let words_only =
        |text: &str| -> String { text.chars().filter(|c| !glyphs.contains(c)).collect() };
    for density in [
        Density::Glyphs,
        Density::Compact,
        Density::Short,
        Density::Full,
    ] {
        let mut seen = std::collections::BTreeSet::new();
        let mut seen_words = std::collections::BTreeSet::new();
        for axes in all_axes() {
            let text: String = match density {
                Density::Full => trust::lines(&axes)
                    .iter()
                    .map(|l| {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => trust::spans(&axes, density)
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect(),
            };
            assert!(seen.insert(text.clone()), "{density:?}: twice {text:?}");
            // Compact trägt nur das Integritätswort — die übrigen Achsen
            // unterscheiden dort die Glyphen.
            if matches!(density, Density::Short | Density::Full) {
                let words = words_only(&text);
                assert!(
                    seen_words.insert(words.clone()),
                    "{density:?}: words twice {words:?}"
                );
            }
            // Drei Slots, je ein Glyph aus dem Register, epistemisch.
            let slots = trust::slots(&axes);
            for slot in &slots {
                let entry = theme::REGISTER.iter().find(|e| e.symbol == slot.glyph);
                assert_eq!(
                    entry.map(|e| e.class),
                    Some(theme::GlyphClass::Epistemic),
                    "{}",
                    slot.glyph
                );
            }
            if density == Density::Glyphs {
                assert_eq!(
                    text.chars().filter(|c| !c.is_whitespace()).count(),
                    3,
                    "{text}"
                );
            }
            if density == Density::Short {
                // Feste Reihenfolge: Integrität · Coverage · Deutung.
                let at = |w: &str| text.find(w).unwrap_or_else(|| panic!("{w} in {text}"));
                assert!(at(&slots[0].word) < at(&slots[1].word), "{text}");
                assert!(at(&slots[1].word) < at(&slots[2].word), "{text}");
                assert_eq!(text.matches(" · ").count(), 2, "{text}");
            }
        }
    }
}

/// Liste (Compact), Fußzeile (Short) und Graph-Kopf (Short) zeigen dieselbe
/// Komponente — die SEAL-Spalte behält ihr Wort (Klasse 1: „Legacy bleibt
/// Legacy").
#[test]
fn a4_list_footer_and_graph_head_show_the_trust_strip() {
    for (w, h) in SIZES {
        let (_dir, repo) = repo();
        let mut app = App::new(filled(), &repo, None);
        let out = render_sized(&mut app, w, h);
        assert!(out.contains("◈ sealed ● ✓"), "{w}x{h}\n{out}");
        assert!(out.contains("· legacy · ✓"), "{w}x{h}\n{out}");
        let footer: String = out.lines().rev().take(2).collect::<Vec<_>>().join("\n");
        assert!(footer.contains("◈ sealed"), "{w}x{h}\n{out}");
        if w >= 120 {
            assert!(
                footer.contains("◈ sealed · ● complete · ✓ interpreted"),
                "{w}x{h}\n{out}"
            );
        }
        app.reduce(Action::Enter);
        let out = render_sized(&mut app, w, h);
        // Die Zeile unter `SESSION …` ist der Kopf: Ergebnis zuerst — der
        // Vertrauenszustand vor Akteur und Umfang (die bei 80 Spalten als
        // Erstes weichen).
        let lines: Vec<&str> = out.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("SESSION b3-aaaaaaaa"))
            .unwrap_or_else(|| panic!("{w}x{h}\n{out}"));
        let head = lines[at + 1];
        let start = head
            .find("◈ sealed")
            .unwrap_or_else(|| panic!("{w}x{h}\n{out}"));
        if let Some(actor) = head.find("claude-code") {
            assert!(start < actor, "{w}x{h}\n{head}");
        }
    }
}

/// Review „open" ist der Normalzustand: in Liste und Übersicht leise.
#[test]
fn a4_an_open_review_is_quiet_in_lists() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    // Breit genug für die VERDICT-Spalte.
    let out = render_sized(&mut app, 300, 30);
    assert!(out.contains("VERDICT"), "{out}");
    assert!(out.contains("↻ needs work"), "{out}");
    assert!(!out.contains("⚠ open"), "{out}");
}

/// Keine eigenen Seal-/Verdict-Strings in `view/` außerhalb von `trust.rs`
/// und `theme.rs`.
#[test]
fn a4_no_own_trust_strings_outside_the_component() {
    let sources = [
        ("activity.rs", include_str!("../activity.rs")),
        ("evidence.rs", include_str!("../evidence.rs")),
        ("graph.rs", include_str!("../graph.rs")),
        ("mod.rs", include_str!("../mod.rs")),
        ("overview.rs", include_str!("../overview.rs")),
        ("verify.rs", include_str!("../verify.rs")),
        ("why.rs", include_str!("../why.rs")),
        ("intent.rs", include_str!("../intent.rs")),
        ("changes.rs", include_str!("../changes.rs")),
    ];
    let forbidden = [
        "TAMPERED",
        "tampered\"",
        "VERIFIED",
        "hash-valid",
        "\"◈",
        "◈ sealed",
        "legacy\"",
        "incomplete\"",
        "uninterpreted\"",
        "interpreted\"",
    ];
    for (file, source) in sources {
        // Nur Code, keine Kommentare.
        for (n, line) in source.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for word in forbidden {
                assert!(
                    !code.contains(word),
                    "view/{file}:{}: {word:?} belongs in trust.rs\n{line}",
                    n + 1
                );
            }
        }
    }
}
