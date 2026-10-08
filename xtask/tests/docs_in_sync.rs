//! Die Stufen-Tabelle in `docs/verification-guide.md` ist genau die, die
//! `cargo xtask proof-table` aus dem Proof-Vokabular erzeugt (EA-21): Keine
//! Doku-Zeile behauptet mehr — oder weniger — als `minds-core` für die
//! Stufe.

use std::path::Path;

#[test]
fn verification_guide_carries_the_generated_proof_table() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/verification-guide.md");
    // Ein Checkout mit `core.autocrlf=true` (Windows) liefert `\r\n` — das
    // ist kein Drift.
    let doc = std::fs::read_to_string(&path)
        .expect("verification guide")
        .replace("\r\n", "\n");
    let block = xtask::generated_block(&doc)
        .expect("the guide carries exactly one generated proof-table block");
    assert_eq!(
        block,
        xtask::proof_table().trim_end_matches('\n'),
        "docs/verification-guide.md is out of date — regenerate with `cargo xtask proof-table`"
    );
}
