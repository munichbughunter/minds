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

// ---------------------------------------------------------------------
// B5 · Fokus beim Zoom- und Ansichtswechsel erhalten
// ---------------------------------------------------------------------

fn focused_kind(app: &App) -> Option<String> {
    match app.top() {
        Some(View::Graph { rows, cursor, .. }) => rows
            .get(*cursor)
            .map(|r| format!("{:?} {}", r.kind, r.label)),
        _ => None,
    }
}

#[test]
fn b5_zoom_and_timeline_keep_the_focused_node() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    // YOU, AGENT, READ, EDIT: der Cursor auf EDIT.
    for _ in 0..3 {
        app.reduce(Action::Down);
    }
    let edit = focused_kind(&app).unwrap();
    assert!(edit.contains("Edit"), "{edit}");
    // Verbose fügt Züge davor ein — der Cursor folgt dem Knoten, nicht dem Index.
    app.reduce(Action::Zoom(3));
    assert_eq!(focused_kind(&app).as_deref(), Some(edit.as_str()));
    app.reduce(Action::Zoom(1));
    assert!(
        focused_kind(&app).unwrap().contains("Edit"),
        "{:?}",
        focused_kind(&app)
    );
    app.reduce(Action::Zoom(2));
    assert_eq!(focused_kind(&app).as_deref(), Some(edit.as_str()));
    // Baum ↔ Zeitleiste: derselbe Knoten.
    app.reduce(Action::ToggleTimeline);
    assert_eq!(focused_kind(&app).as_deref(), Some(edit.as_str()));
    app.reduce(Action::ToggleTimeline);
    assert_eq!(focused_kind(&app).as_deref(), Some(edit.as_str()));
}

/// Ein ausgeblendeter Zug (Zoom 3 → 2) landet auf der Zeile, die ihn
/// enthält — seinem sichtbaren Vorfahren.
#[test]
fn b5_a_hidden_node_lands_on_the_row_that_contains_it() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    app.reduce(Action::Zoom(3));
    let turn = match app.top() {
        Some(View::Graph { rows, .. }) => rows
            .iter()
            .position(|r| matches!(r.kind, minds_reader::graph::NodeKind::Turn(_)))
            .expect("a turn row"),
        _ => unreachable!(),
    };
    for _ in 0..turn {
        app.reduce(Action::Down);
    }
    app.reduce(Action::Zoom(2));
    assert!(
        focused_kind(&app).unwrap().contains("Agent"),
        "{:?}",
        focused_kind(&app)
    );
}

// ---------------------------------------------------------------------
// B4 · Konsistente Tasten
// ---------------------------------------------------------------------

#[test]
fn b4_z_and_shift_z_step_the_zoom() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Enter);
    assert_eq!(app.zoom, crate::Zoom::Normal);
    app.reduce(Action::ZoomStep(true));
    assert_eq!(app.zoom, crate::Zoom::Verbose);
    app.reduce(Action::ZoomStep(true));
    assert_eq!(app.zoom, crate::Zoom::Verbose, "finest stays");
    app.reduce(Action::ZoomStep(false));
    app.reduce(Action::ZoomStep(false));
    assert_eq!(app.zoom, crate::Zoom::Summary);
    app.reduce(Action::ZoomStep(false));
    assert_eq!(app.zoom, crate::Zoom::Summary, "coarsest stays");
    // Breit genug, dass die Fußzeile den Zoom-Hinweis nicht kürzt.
    let out = render_sized(&mut app, 300, 30);
    assert!(out.contains("z Z zoom"), "{out}");
    assert!(!out.contains("1·2·3 zoom"), "{out}");
}

/// Dieselben Verben auf jedem Element mit Session oder Commit; was nicht
/// passt, sagt es, statt still nichts zu tun.
#[test]
fn b4_the_object_verbs_work_on_a_session() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    // Session a trägt Commit 1.
    app.reduce(Action::Verify);
    assert_eq!(app.tab, crate::app::Tab::Verify);
    assert_eq!(app.verify.as_ref().unwrap().commit(), Some(commit('1')));
    app.reduce(Action::Back); // B2: zurück
    assert_eq!(app.tab, crate::app::Tab::Sessions);
    app.reduce(Action::Diff);
    assert_eq!(app.tab, crate::app::Tab::Changes);
    assert_eq!(app.changes.as_ref().unwrap().commit(), Some(commit('1')));
    app.reduce(Action::Back);
    assert_eq!(app.tab, crate::app::Tab::Sessions);
    // Kein Intent-Anker: ein Hinweis, kein stiller Nicht-Effekt.
    app.reduce(Action::Intent);
    assert_eq!(app.tab, crate::app::Tab::Sessions);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("no intent anchor"), "{out}");
    // Session b hat keinen Commit: v und d sagen es.
    app.reduce(Action::Down);
    app.reduce(Action::Verify);
    assert_eq!(app.tab, crate::app::Tab::Sessions);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("no commit"), "{out}");
}

/// Auch in Randfällen nie still nichts: `w` ohne erklärbaren Gegenstand und
/// `c` außerhalb von Changes sagen, warum nicht.
#[test]
fn b4_verbs_without_an_object_say_why() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::TabTo(4)); // Intent: kein Anker, keine Session
    app.reduce(Action::Why);
    assert!(app.closed.is_some(), "w in an empty Intent tab");
    app.reduce(Action::TabTo(1));
    app.reduce(Action::Context);
    assert!(app.closed.is_some(), "c outside Changes");
    // Changes ohne Zeile: w sagt es.
    app.reduce(Action::TabTo(3));
    app.reduce(Action::Why);
    assert!(
        app.closed.is_some() || matches!(app.top(), Some(View::Why { .. })),
        "w in Changes"
    );
}

// ---------------------------------------------------------------------
// C1 · Glossar an einer Stelle, Erklärung im Kontext
// ---------------------------------------------------------------------

#[test]
fn c1_the_glossary_says_each_word_in_one_sentence() {
    use crate::copy::{GLOSSARY, sentence};
    for term in GLOSSARY {
        assert!(!term.sentence.is_empty(), "{}", term.word);
        assert!(term.sentence.ends_with('.'), "{}: one sentence", term.word);
        assert_eq!(
            term.sentence.matches(". ").count(),
            0,
            "{}: one sentence",
            term.word
        );
    }
    // Die Pflicht-Unterscheidungen.
    for (word, says) in [
        ("sealed", "not verified"),
        ("observed", "not checked"),
        ("claim", "not evidence"),
        ("not observed", "not wrong"),
        ("incomplete", "not tampered"),
    ] {
        let s = sentence(word).unwrap_or_else(|| panic!("{word} missing"));
        assert!(s.contains(says), "{word}: {s}");
    }
    // Fachjargon ist erklärt.
    for word in [
        "hash-valid",
        "epoch",
        "witness",
        "conformance",
        "A0",
        "A1",
        "A2",
        "A3",
    ] {
        assert!(sentence(word).is_some(), "{word} missing");
    }
}

/// Die Hilfe nutzt dasselbe Glossar.
#[test]
fn c1_the_help_carries_the_glossary() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.reduce(Action::Help);
    app.reduce(Action::End);
    let mut seen = String::new();
    for _ in 0..80 {
        seen.push_str(&render_sized(&mut app, 160, 40));
        app.reduce(Action::Up);
    }
    assert!(seen.contains("Glossary"), "{seen}");
    // Lange Sätze bricht die Hilfe um — der Anfang steht auf einer Zeile.
    for word in ["sealed", "hash-valid"] {
        let head: String = crate::copy::sentence(word)
            .unwrap()
            .chars()
            .take(40)
            .collect();
        assert!(seen.contains(&head), "{word}");
    }
}

/// Zeile 1 der Fußzeile erklärt den fokussierten Zustand — in jeder Ansicht.
#[test]
fn c1_footer_line_one_explains_the_focused_state() {
    use crate::copy::sentence;
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let footer = |out: &str| out.lines().rev().nth(1).unwrap_or("").to_string();
    // Liste: Evidenz-Satz bleibt vorn, der Seal-Satz folgt.
    let out = render_sized(&mut app, 300, 30);
    assert!(
        footer(&out).contains("Observed: the commit carries the"),
        "{out}"
    );
    assert!(footer(&out).contains(sentence("sealed").unwrap()), "{out}");
    // Graph auf YOU: der Auftrag ist eine Aussage.
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 300, 30);
    assert!(footer(&out).contains(sentence("claim").unwrap()), "{out}");
    app.reduce(Action::Back);
    // Verify: das Urteilswort mit seinem Satz.
    app.reduce(Action::TabTo(2));
    let out = render_sized(&mut app, 300, 30);
    assert!(
        !footer(&out).contains("at a glance"),
        "static sentence gone\n{out}"
    );
    assert!(
        crate::copy::GLOSSARY
            .iter()
            .any(|t| footer(&out).contains(t.sentence)),
        "{out}"
    );
}

// ---------------------------------------------------------------------
// C4 · Zeiten
// ---------------------------------------------------------------------

#[test]
fn c4_relative_times_in_lists_absolute_in_details() {
    use super::super::{ago, when};
    let at = "2026-10-14T09:00:00Z";
    let base = minds_metrics::epoch_seconds(at).unwrap();
    assert_eq!(ago(Some(at), base + 30), "just now");
    assert_eq!(ago(Some(at), base + 5 * 60), "5m ago");
    assert_eq!(ago(Some(at), base + 3 * 3600), "3h ago");
    assert_eq!(ago(Some(at), base + 2 * 86_400), "2d ago");
    // In der Zukunft oder unlesbar: die absolute Form, nie ein Unsinn.
    assert_eq!(ago(Some(at), base - 3600), "2026-10-14 09:00 UTC");
    assert_eq!(ago(Some("kaputt"), base), "kaputt");
    assert_eq!(ago(None, base), "—");
    assert_eq!(when(Some(at)), "2026-10-14 09:00 UTC");
}

#[test]
fn c4_the_list_shows_relative_times() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let started = minds_metrics::epoch_seconds("2026-07-25T14:10:00Z").unwrap();
    app.now = std::time::UNIX_EPOCH + std::time::Duration::from_secs((started + 3 * 3600) as u64);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("3h ago"), "{out}");
    assert!(!out.contains("25.07. 14:10Z"), "{out}");
    // Im Detail (Graph-Kopf) die absolute Zeit in UTC.
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains("2026-07-25 14:10 UTC"), "{out}");
}

// ---------------------------------------------------------------------
// C5 · Begriffe, die kollidieren
// ---------------------------------------------------------------------

#[test]
fn c5_terms_no_longer_collide() {
    use super::super::tokens;
    assert_eq!(tokens(999), "999 tok");
    assert_eq!(tokens(5_700), "5.7k tok");
    assert_eq!(tokens(1_234_567), "1.2M tok");
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    let out = render_sized(&mut app, 300, 30);
    // Der Anteil der Commits mit Session ist nicht die Coverage-Achse.
    assert!(out.contains("% of commits with a session"), "{out}");
    assert!(!out.contains("context coverage"), "{out}");
    assert!(out.contains("1 file · 0 tok"), "{out}");
    assert!(!out.contains(" D · "), "{out}");
    let (text, _) = super::super::trust::verify_headline(crate::verify::Overall::NotChecked);
    assert!(text.contains("NOT CHECKED — run minds verify"), "{text}");
    assert!(text.contains("press v"), "{text}");
}

// ---------------------------------------------------------------------
// C7 · Graph-Lesbarkeit
// ---------------------------------------------------------------------

fn busy(calls: usize, path: &str) -> Inspection {
    let mut s = session("Touch one file many times", "2026-07-25T14:10:00Z");
    let call = s.turns[0].tool_calls[0].clone();
    for _ in 0..calls {
        let mut c = call.clone();
        if let Some(effect) = c.effect.as_mut() {
            effect.path = Some(path.into());
        }
        s.turns[0].tool_calls.push(c);
    }
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    Inspection::from_index(
        Index::from_parts(sessions, BTreeMap::new()),
        Vec::new(),
        "repo",
    )
}

#[test]
fn c7_a_large_graph_starts_in_summary_and_says_more_is_there() {
    let (_dir, repo) = repo();
    let mut app = App::new(busy(45, "src/lib.rs"), &repo, None);
    app.reduce(Action::Enter);
    assert_eq!(
        app.zoom,
        crate::Zoom::Summary,
        "more than 40 rows at zoom 2"
    );
    let out = render_sized(&mut app, 160, 40);
    let footer: String = out.lines().rev().take(2).collect::<Vec<_>>().join("\n");
    assert!(footer.contains("z shows"), "{footer}");
    // Ein kleiner Graph bleibt in der Normalstufe.
    let (_small_dir, small_repo) = super::repo();
    let mut app = App::new(filled(), &small_repo, None);
    app.reduce(Action::Enter);
    assert_eq!(app.zoom, crate::Zoom::Normal);
}

#[test]
fn c7_labels_use_the_actual_width() {
    // 80 Zeichen: Der pauschale Abzug ließ bei 100 Spalten 70 frei (kürzte
    // ihn), die tatsächliche Breite lässt 87 (zeigt ihn ganz).
    let long = format!("src/{}.rs", "x".repeat(73));
    assert_eq!(long.chars().count(), 80);
    let (_dir, repo) = repo();
    let mut app = App::new(busy(1, &long), &repo, None);
    app.reduce(Action::Enter);
    let out = render_sized(&mut app, 100, 30);
    assert!(out.contains(&long), "{long} clipped too early\n{out}");
}

/// A6: Neben der Vorschau bekommt die Überschrift bei 120 Spalten mindestens
/// 30 Zeichen — die Spalten ringsum sind schmaler geworden (relative Zeit,
/// Trust Strip), nicht die Zustände.
#[test]
fn a6_the_list_headline_gets_thirty_columns_at_120() {
    let mut s = session(&"x".repeat(60), "2026-07-25T14:10:00Z");
    s.intent.request = "y".repeat(60);
    let mut sessions = BTreeMap::new();
    sessions.insert(sid('a'), s);
    let inspection = Inspection::from_index(
        Index::from_parts(sessions, BTreeMap::new()),
        Vec::new(),
        "repo",
    );
    let (_dir, repo) = repo();
    let mut app = App::new(inspection, &repo, None);
    let out = render_sized(&mut app, 120, 30);
    assert!(
        out.contains(" SESSION b3-"),
        "the preview stands beside the list\n{out}"
    );
    let row = out
        .lines()
        .find(|l| l.contains("yyyy"))
        .unwrap_or_else(|| panic!("{out}"));
    let shown = row.chars().filter(|c| *c == 'y').count();
    assert!(shown >= 29, "headline {shown}+… < 30 columns\n{out}");
}

/// C7 · Geprüft: Reichen Glyph + Pfad in Zoom 1/2? Nein — ohne Farbe trägt
/// das Wort die Art mit (Regel „Farbe trägt nie allein": Glyph **und**
/// Wort). Die Zeilen behalten daher in jeder Stufe Glyph und Wort, auch
/// monochrom.
#[test]
fn c7_graph_rows_keep_glyph_and_word_in_every_zoom_even_without_color() {
    let (_dir, repo) = repo();
    let mut app = App::new(filled(), &repo, None);
    app.no_color = true;
    app.reduce(Action::Enter);
    for zoom in [1u8, 2] {
        app.reduce(Action::Zoom(zoom));
        let out = render_sized(&mut app, 160, 40);
        for (kind, label) in [
            (minds_reader::graph::ToolKind::Read, "src/http/retry.rs"),
            (minds_reader::graph::ToolKind::Edit, "src/http/retry.rs"),
            (minds_reader::graph::ToolKind::Exec, "cargo test"),
        ] {
            let (glyph, word, _) = theme::tool(kind);
            let row = format!("{glyph} {word} {label}");
            // In der Übersicht trägt eine zusammengefasste Zeile `×N`.
            let merged = format!("{glyph} {word} {label} ×");
            assert!(
                out.contains(&row) || out.contains(&merged),
                "zoom {zoom}: {row}\n{out}"
            );
        }
    }
}
