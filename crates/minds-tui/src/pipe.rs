//! Die Ausgabe, wenn stdout kein Terminal ist: eine Zeile je Karte,
//! tab-separiert, ohne ANSI — für `| grep`, `| fzf`, `| cut`.
//!
//! Mensch bekommt die Oberfläche, Pipe bekommt Zeilen; beide aus derselben
//! Liste, mit derselben Suche. Die Strings sind bereits entschärft
//! (Tabulator und Zeilenumbruch sichtbar gemacht), also bleibt eine Zeile
//! eine Zeile.

use std::io::Write;

use minds_reader::model::{SessionCard, WhyChain, WhyStep};

/// Schreibt die Karten.
pub fn cards(out: &mut impl Write, cards: &[SessionCard]) -> std::io::Result<()> {
    for card in cards {
        let (_, evidence, _) = crate::theme::evidence(card.evidence);
        let (_, seal_verdict, _) = crate::theme::provenance(&card.provenance);
        let changes: Vec<String> = card.changes.iter().map(|c| c.to_string()).collect();
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            card.started_at.as_deref().unwrap_or("-"),
            card.id,
            card.summary.actor,
            card.summary.files,
            card.summary.input_tokens,
            card.summary.output_tokens,
            evidence,
            seal_verdict,
            card.review.verdict.word(),
            if changes.is_empty() {
                "-".to_string()
            } else {
                changes.join(",")
            },
            card.summary.headline,
        )?;
    }
    Ok(())
}

/// Schreibt eine Herkunftskette als `schritt\twert`-Zeilen.
pub fn why(out: &mut impl Write, chain: &WhyChain) -> std::io::Result<()> {
    for step in &chain.steps {
        match step {
            WhyStep::Line { path, line } => writeln!(out, "line\t{path}:{line}")?,
            WhyStep::Commit { id, subject } => writeln!(
                out,
                "commit\t{}\t{}",
                id.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                subject.as_deref().unwrap_or("")
            )?,
            WhyStep::Change { id } => writeln!(
                out,
                "change\t{}",
                id.as_ref()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "-".into())
            )?,
            WhyStep::Sessions { cards } => {
                if cards.is_empty() {
                    writeln!(out, "session\t-")?;
                }
                for card in cards {
                    writeln!(out, "session\t{}\t{}", card.id, card.summary.headline)?;
                }
            }
            WhyStep::Agent {
                name,
                version,
                model,
            } => writeln!(out, "agent\t{name} {version}\t{model}")?,
            WhyStep::Intent { request, .. } => writeln!(out, "intent\t{request}")?,
            WhyStep::Evidence { links } => {
                for link in links {
                    let (_, word, _) = crate::theme::evidence(Some(link.evidence));
                    writeln!(out, "evidence\t{}\t{}\t{word}", link.commit, link.session)?;
                }
            }
            WhyStep::Review { state } => writeln!(out, "review\t{}", state.verdict.word())?,
        }
    }
    // Die Lücken zuletzt — eine Zeile je Lücke, damit `grep '^gap'` reicht.
    for gap in chain.gaps() {
        writeln!(out, "gap\t{:?}\t{}", gap.kind, gap.text)?;
    }
    Ok(())
}
