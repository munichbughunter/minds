//! Werkzeuge für das Repository selbst (`cargo xtask …`), nie Teil des
//! ausgelieferten Binaries.
//!
//! Die Doku soll nicht mehr behaupten als der Code: Was eine Assurance-Stufe
//! belegt und was nicht, steht genau einmal — im Proof-Vokabular von
//! `minds-core` (EA-13). Die Tabelle in `docs/verification-guide.md` wird
//! hier daraus erzeugt, und ein Test hält sie mit dem Code gleich (EA-21).

use minds_core::evidence::{DOES_NOT_PROVE_V2, Level, PROVES_V2, ProofSentence};

/// Die Marke, ab der die erzeugte Tabelle in der Doku steht.
pub const BEGIN_MARKER: &str =
    "<!-- BEGIN generated: cargo xtask proof-table — do not edit by hand -->";
/// Die Marke, an der sie endet.
pub const END_MARKER: &str = "<!-- END generated: cargo xtask proof-table -->";

/// Die Stufen-Tabelle als Markdown, ohne Marken: je ein Abschnitt für
/// „proves" und „does not prove", eine Zeile je Satz, eine Spalte je Stufe.
pub fn proof_table() -> String {
    let mut out = String::new();
    section(
        &mut out,
        "Proves — ✓: the promise holds at that level",
        PROVES_V2,
    );
    out.push('\n');
    section(
        &mut out,
        "Does not prove — ✓: the limit still applies at that level",
        DOES_NOT_PROVE_V2,
    );
    out
}

/// Ein Abschnitt: Kopf, Trennzeile, Sätze in der Reihenfolge des Vokabulars.
fn section(out: &mut String, title: &str, sentences: &[ProofSentence]) {
    out.push_str(&format!("**{title}**\n\n"));
    // Kopf und Zellen aus derselben Stufenliste: Eine neue Stufe kann die
    // Tabelle nicht verschieben.
    let levels: Vec<&str> = Level::ALL.iter().map(|level| label(*level)).collect();
    out.push_str(&format!("| id | {} | sentence |\n", levels.join(" | ")));
    out.push_str(&format!("|---|{}---|\n", ":-:|".repeat(levels.len())));
    for sentence in sentences {
        let marks: Vec<&str> = Level::ALL
            .iter()
            .map(|level| {
                if sentence.holds_at(*level) {
                    "✓"
                } else {
                    "–"
                }
            })
            .collect();
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            sentence.id,
            marks.join(" | "),
            cell(sentence.text)
        ));
    }
}

/// Die Spaltenüberschrift einer Stufe — ausdrücklich, nicht aus `Debug`.
/// Der `match` ist erschöpfend: Eine neue Stufe bricht hier den Build, bis
/// sie eine Überschrift hat.
fn label(level: Level) -> &'static str {
    match level {
        Level::A0 => "A0",
        Level::A1 => "A1",
        Level::A2 => "A2",
        Level::A3 => "A3",
    }
}

/// Ein Satz als Tabellenzelle: `|` und Zeilenumbrüche würden die Zeile
/// sprengen.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// Der Text zwischen den Marken in `doc`, ohne die umgebenden Leerzeilen —
/// `None`, wenn eine Marke fehlt, doppelt oder verkehrt herum steht.
pub fn generated_block(doc: &str) -> Option<&str> {
    if doc.matches(BEGIN_MARKER).count() != 1 || doc.matches(END_MARKER).count() != 1 {
        return None;
    }
    let start = doc.find(BEGIN_MARKER)? + BEGIN_MARKER.len();
    let end = doc.find(END_MARKER)?;
    (start <= end).then(|| doc[start..end].trim_matches('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sentence_has_exactly_one_row() {
        let table = proof_table();
        for sentence in PROVES_V2.iter().chain(DOES_NOT_PROVE_V2) {
            let row = format!("| `{}` |", sentence.id);
            assert_eq!(table.matches(&row).count(), 1, "{}", sentence.id);
        }
    }

    #[test]
    fn the_append_window_limit_ends_at_a2() {
        let table = proof_table();
        let row = table
            .lines()
            .find(|line| line.starts_with("| `append_to_seal_window` |"))
            .expect("row");
        assert!(row.starts_with("| `append_to_seal_window` | ✓ | ✓ | – | – |"));
    }

    #[test]
    fn the_block_between_the_markers_is_found_once() {
        let doc = format!("intro\n\n{BEGIN_MARKER}\n\nX\n\n{END_MARKER}\n");
        assert_eq!(generated_block(&doc), Some("X"));
        let twice = format!("{doc}{BEGIN_MARKER}");
        assert_eq!(generated_block(&twice), None);
        let reversed = format!("{END_MARKER}\n{BEGIN_MARKER}");
        assert_eq!(generated_block(&reversed), None);
        assert_eq!(generated_block("no markers"), None);
    }
}
