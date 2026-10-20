//! C12.3 · Requirements-Tab: die Anforderung zuerst, dann ihr Status mit
//! einem Satz je Zeile, dann die Sessions, Technik zuletzt — eingeklappt.

use minds_core::Model;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::view::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

/// Ein belegter, aber unsignierter Anker.
fn proven_unsigned(byte: u8) -> IntentInfo {
    let mut info = proven(byte);
    if let Ok(detail) = &mut info.detail {
        detail.signature = IntentSignature::Unsigned;
    }
    info
}

fn at(out: &str, needle: &str) -> usize {
    out.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} missing in:\n{out}"))
}

/// C12.3-AC1: Die Liste liest sich als Satz — Status-Glyph, Kurz-Id (wie
/// die Overview), Quelle mit HEAD-Hinweis, Scope, Sessions als Zahl.
#[test]
fn c123_the_list_reads_status_id_source_scope_and_sessions() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(
        out.contains("✓ 01010101 · docs/req.md (HEAD) · src/sort/**, tests/** · 1 session"),
        "{out}"
    );
    // Keine Pille, keine lange Id in der Liste.
    assert!(!out.contains("◉1"), "{out}");
    let row = out.lines().find(|l| l.contains("▸ ✓")).unwrap();
    assert!(!row.contains("b3-"), "{row}");
}

/// C12.3-AC2: Ein Glyph je Status — `✓` belegt und signiert, `◦` belegt,
/// nicht signiert, `✗` Snapshot passt nicht; Glyph und Wort, nie nur Farbe.
#[test]
fn c123_one_glyph_per_status() {
    let (_dir, repo) = repo();
    let source = source();
    source
        .store
        .replace(vec![proven(1), proven_unsigned(4), unproven(2)]);
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(out.contains("✓ 01010101"), "{out}");
    assert!(out.contains("◦ 04040404"), "{out}");
    assert!(out.contains("✗ 02020202"), "{out}");
}

/// C12.3-AC3: Das Detail hat eine feste Reihenfolge — REQUIREMENT (der
/// Text oben), STATUS, SESSIONS, TECHNICAL.
#[test]
fn c123_the_detail_order_is_requirement_status_sessions_technical() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    let requirement = at(&out, "⚑ 01010101");
    let text = at(&out, "AC1: sort by date");
    let status = at(&out, "STATUS");
    let sessions = at(&out, "SESSIONS (1)");
    let technical = at(&out, "TECHNICAL");
    assert!(requirement < text, "{out}");
    assert!(text < status, "{out}");
    assert!(status < sessions, "{out}");
    assert!(sessions < technical, "{out}");
    assert!(out.contains("… cut here — minds intent show"), "{out}");
}

/// C12.3-AC4: STATUS — drei Zeilen, jede mit einem Satz: belegt, signiert,
/// Bindung (A1).
#[test]
fn c123_status_lines_carry_a_sentence_each() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(
        out.contains("✓ proven — the stored snapshot matches the file at 3f9c1e2a"),
        "{out}"
    );
    assert!(
        out.contains(
            "✓ signed — software key, checked against your allowed_signers (as minds verify does)"
        ),
        "{out}"
    );
    assert!(
        out.contains("· binding A1 observed — the agent named this anchor in a witness event"),
        "{out}"
    );
    // Die alten Rohzeilen sind weg.
    assert!(!out.contains("proof     ok"), "{out}");
    assert!(!out.contains("active in the local file (A1)"), "{out}");
}

/// C12.3-AC5: Nicht signiert ist offen (`◦`), nie ein Fehler; nicht belegt
/// ist `✗` mit dem Grund — und der Text bleibt verborgen (Klasse 1).
#[test]
fn c123_not_signed_is_open_and_not_proven_hides_the_text() {
    let (_dir, repo) = repo();
    let source = source();
    source.store.replace(vec![proven_unsigned(4), unproven(2)]);
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(out.contains("◦ not signed —"), "{out}");
    assert!(!out.contains("✗ not signed"), "{out}");
    assert!(out.contains("minds intent sign"), "{out}");
    app.reduce(Action::Down);
    let out = render(&mut app);
    assert!(
        out.contains("✗ not proven — intent snapshot does not match its anchor"),
        "{out}"
    );
    assert!(
        out.contains("text hidden — the anchor is not proven"),
        "{out}"
    );
    assert!(!out.contains("PLANTED_SECRET"), "{out}");
    assert!(!out.contains("Snapshot (11 bytes) not shown"), "{out}");
    assert!(out.contains("· binding A1 observed —"), "{out}");
}

/// C12.3-AC6: TECHNICAL ist eingeklappt — Space zeigt Anker, Inhalts-Hash
/// und Quelle in voller Länge, Space wieder versteckt sie.
#[test]
fn c123_space_unfolds_the_technical_block() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    let full = anchor(1).to_string();
    let content = anchor(9).to_string();
    assert!(!out.contains("anchor    b3-"), "{out}");
    assert!(!out.contains(&content), "{out}");
    assert!(!out.contains("file:docs/req.md@3f9c1e2a"), "{out}");
    app.reduce(Action::Space);
    let out = render_sized(&mut app, 160, 40);
    assert!(out.contains(&format!("anchor    {full}")), "{out}");
    assert!(out.contains(&content), "{out}");
    assert!(out.contains("file:docs/req.md@3f9c1e2a"), "{out}");
    assert!(out.contains("proof     ok — snapshot matches"), "{out}");
    assert!(out.contains("valid (software key)"), "{out}");
    app.reduce(Action::Space);
    let out = render_sized(&mut app, 160, 40);
    assert!(!out.contains(&content), "{out}");
}

/// C12.3-AC7: SESSIONS nennt Agent, Modell, Session und den Auftrag als
/// Claim — wie das Overview-Detail; kein Platzhalter wird gedruckt.
#[test]
fn c123_sessions_name_agent_model_and_the_request_as_a_claim() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(out.contains("SESSIONS (1)"), "{out}");
    assert!(out.contains("◉CC claude-code · opus"), "{out}");
    assert!(out.contains("(witness event)"), "{out}");
    assert!(out.contains("◌ Fix retry handling"), "{out}");
    assert!(!out.contains("Sessions naming this anchor"), "{out}");

    // Ein Modell, das die Erfassung nicht kannte: nie `unknown`.
    let mut inspection = anchored();
    let mut sessions: Vec<_> = inspection.index().sessions().map(|(id, _)| *id).collect();
    sessions.sort();
    let mut index_sessions = BTreeMap::new();
    for id in sessions {
        let mut s = inspection.index().session(id).unwrap().clone();
        s.model = Model {
            provider: "unknown".into(),
            id: "unknown".into(),
        };
        index_sessions.insert(id, s);
    }
    let mut commits = BTreeMap::new();
    commits.insert(commit('1'), vec![sid('a')]);
    inspection =
        Inspection::from_index(Index::from_parts(index_sessions, commits), Vec::new(), "t");
    let mut app = App::new(inspection, &repo, None);
    app.reduce(Action::TabTo(4));
    app.fill_intents(&source);
    let out = render(&mut app);
    assert!(!out.contains("unknown"), "{out}");
    assert!(out.contains("model not recorded"), "{out}");
}

/// C12.3-AC8: Keine Tastenhinweise im Detail; die Fußzeile nennt den
/// Anker mit seinem Status (Zeile 1) und die Tasten (Zeile 2).
#[test]
fn c123_no_keys_in_the_detail_and_the_footer_names_anchor_and_status() {
    let (_dir, repo) = repo();
    let source = source();
    let mut app = opened(&repo, &source);
    let out = render(&mut app);
    assert!(!out.contains("ANCHOR — PgUp/PgDn scroll"), "{out}");
    assert!(out.contains("01010101 · ✓ proven · ✓ signed"), "{out}");
    assert!(out.contains("Space technical"), "{out}");
}

/// C12.3-AC9: Bei 80, 120 und 160 Spalten bleiben Status und Kurz-Id
/// sichtbar, `? help` steht, nichts gerät durcheinander.
#[test]
fn c123_renders_at_80_120_and_160_columns() {
    let (_dir, repo) = repo();
    let source = source();
    for (width, height) in [(80, 24), (120, 30), (160, 40)] {
        let mut app = opened(&repo, &source);
        let out = render_sized(&mut app, width, height);
        assert!(out.contains("✓ 01010101"), "{width}: {out}");
        assert!(out.contains("? help"), "{width}: {out}");
        assert!(out.contains("STATUS"), "{width}: {out}");
        assert!(out.contains("✓ proven"), "{width}: {out}");
    }
}
