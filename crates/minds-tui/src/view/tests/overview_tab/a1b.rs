//! A1(b) · Overview-Badges und -Pillen aus dem Glyph-Register.

use super::*;
use crate::theme;
use crate::theme::GlyphClass::{Epistemic, Structural};
use minds_reader::overview::SealMark;

fn render_sized(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::super::draw(frame, app))
        .unwrap();
    terminal.backend().to_string()
}

fn entry(symbol: &str) -> &'static theme::Glyph {
    theme::REGISTER
        .iter()
        .find(|e| e.symbol == symbol)
        .unwrap_or_else(|| panic!("{symbol:?} is not in the register"))
}

/// Jede Seal-Pille der Übersicht trägt ein epistemisches Glyph aus dem
/// Register — dieselben wie `theme::provenance`, wo die Bedeutung dieselbe
/// ist; „versiegelt, nicht (ganz) signiert" bekommt ein eigenes.
#[test]
fn a1b_seal_pills_come_from_the_register() {
    for (mark, meaning, word) in [
        (SealMark::Sealed, "sealed", "sealed"),
        (SealMark::Unsigned, "unsigned", "sealed · unsigned"),
        (SealMark::Partial, "unsigned", "sealed · first 64 checked"),
        (SealMark::Incomplete, "incomplete", "incomplete"),
        (SealMark::Unsealed, "none", "unsealed"),
        (SealMark::Tampered, "failed", "TAMPERED"),
    ] {
        let (glyph, w, _) = theme::seal(mark);
        let e = entry(glyph);
        assert_eq!(
            (e.class, e.meaning, w),
            (Epistemic, meaning, word),
            "{mark:?}"
        );
    }
    // Dieselbe Bedeutung, dasselbe Glyph wie im Rest der Oberfläche.
    let (incomplete, _, _) = theme::seal(SealMark::Incomplete);
    let (tampered, word, _) = theme::seal(SealMark::Tampered);
    assert_eq!((tampered, word), (theme::tampered().0, theme::tampered().1));
    assert_eq!(incomplete, "!");
}

/// Badge, HEAD-Pille, Commit-Knoten und eine Session ohne Commit sind
/// Arten, keine Beweiszustände: strukturelle Glyphen aus dem Register.
#[test]
fn a1b_badges_refs_and_graph_nodes_are_structural() {
    for (symbol, meaning) in [
        (theme::AGENT_GLYPH, "agent"),
        (theme::HEAD, "head"),
        (theme::PENDING, "pending"),
        (theme::COMMIT_GLYPH, "change"),
    ] {
        let e = entry(symbol);
        assert_eq!((e.class, e.meaning), (Structural, meaning), "{symbol}");
    }
}

#[test]
fn a1b_the_overview_draws_no_epistemic_glyph_as_structure() {
    for (w, h) in [(120, 30), (160, 40), (208, 30)] {
        let (_dir, repo, _, inspection) = history();
        let mut app = opened(&repo, inspection);
        let out = render_sized(&mut app, w, h);
        // Commit-Knoten und HEAD trugen `●` (observed), das Badge `◆`
        // (content-Evidenz) — beides keine Beweiszustände.
        assert!(!out.contains('●'), "{w}x{h}\n{out}");
        assert!(!out.contains('◆'), "{w}x{h}\n{out}");
        assert!(
            out.contains(&format!("{} HEAD", theme::HEAD)),
            "{w}x{h}\n{out}"
        );
        assert!(out.contains(theme::COMMIT_GLYPH));
    }
}
