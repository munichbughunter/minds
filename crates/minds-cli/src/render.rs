//! Eine Session für das Terminal — geteilt von [`crate::show`] und
//! [`crate::why`].
//!
//! Beide Kommandos enden an derselben Stelle: eine [`Session`] liegt vor, sie
//! soll lesbar auf stdout. Damit `show` und `why` nicht zwei Schreibweisen
//! desselben pflegen, steht das Rendern hier — einmal.

use minds_core::{EdgeKind, Endpoint, Evidence, Session, SessionId};
use minds_store::IndexLink;

/// Führt die Verweise eines Commits zusammen: die Trailer (verbindlich,
/// [`Evidence::Observed`]) und die Kanten aus dem Store-Index (heuristisch). Der
/// Trailer gewinnt — eine Session, die schon beobachtet verknüpft ist, wird
/// nicht zusätzlich als vermutet geführt.
///
/// Rein und ohne I/O, damit die Zusammenführung ohne Repository prüfbar ist.
pub fn merge_links(
    trailers: &[SessionId],
    index_links: &[IndexLink],
) -> Vec<(SessionId, Evidence)> {
    let mut out: Vec<(SessionId, Evidence)> = trailers
        .iter()
        .map(|id| (*id, Evidence::Observed))
        .collect();
    for link in index_links {
        if !out.iter().any(|(id, _)| *id == link.session) {
            out.push((link.session, link.evidence));
        }
    }
    out
}

/// Eine Session, die im Baum gezeigt werden soll.
pub struct Shown<'a> {
    /// Die Id der Session.
    pub id: SessionId,
    /// Die Session selbst.
    pub session: &'a Session,
    /// Herkunft der Verknüpfung Commit → Session.
    pub evidence: Evidence,
}

/// Zeichnet die Sessions eines Commits als Baum unter `header` — angelehnt an
/// Werkzeuge wie Dagger: eine Überschrift, darunter je Session ein Ast mit dem
/// Intent als Blickfang und der Herkunft dezent darunter.
///
/// **Kompakt** im Regelfall (eine Intent-Zeile, eine Metazeile, die Zahl der
/// Dateien). `full` klappt alles auf: ganzer Prompt, alle Dateien, Constraints,
/// verworfene Pfade, Kanten.
pub fn tree(header: &str, items: &[Shown], full: bool) {
    println!("{}", bold(header));

    let n = items.len();
    for (i, item) in items.iter().enumerate() {
        let last = i + 1 == n;
        let branch = if last { "╰─" } else { "├─" };
        let cont = if last { "   " } else { "│  " };
        let s = item.session;

        // Der Intent ist der Blickfang.
        let request = if full {
            s.intent.request.clone()
        } else {
            headline(&s.intent.request, 96)
        };
        println!("{branch} {}", bold(&format!("▸ {request}")));

        // Herkunft, dezent.
        let files = s.produced.files.len();
        let fileword = if files == 1 { "Datei" } else { "Dateien" };
        println!(
            "{cont}{}",
            dim(&format!(
                "{} {} · {}/{} · {}/{} Token · {files} {fileword}",
                s.agent.name,
                s.agent.version,
                s.model.provider,
                s.model.id,
                s.usage.input_tokens,
                s.usage.output_tokens,
            ))
        );
        let vermerk = if item.evidence == Evidence::Observed {
            String::new()
        } else {
            format!("  ({})", evidence(item.evidence))
        };
        println!(
            "{cont}{}",
            dim(&format!("{}{vermerk}", short_id(item.id, full)))
        );

        if full {
            for constraint in &s.intent.constraints {
                println!("{cont}  Constraint: {constraint}");
            }
            for discarded in &s.intent.discarded {
                println!("{cont}  Verworfen:  {discarded}");
            }
            for file in &s.produced.files {
                println!("{cont}  {file}");
            }
            for edge in &s.edges {
                println!(
                    "{cont}  Kante: {} → {} ({})",
                    edge_kind(edge.kind),
                    endpoint(&edge.to),
                    evidence(edge.evidence),
                );
            }
        } else if files > 0 {
            println!(
                "{cont}{}",
                dim("minds show --full zeigt Dateien und Prompt")
            );
        }
    }
}

/// Löst die Verweise eines Commits gegen den Store auf und zeichnet den Baum —
/// der gemeinsame Weg von [`crate::show`] und [`crate::why`].
///
/// Übersprungen wird, was keinen Mehrwert hat: Sessions **ohne erfasste
/// Absicht** (der frühere „(kein Prompt erfasst)"-Ballast). Ein Verweis auf eine
/// Session, die der Store nicht hat, wird als Waise gezählt und dezent
/// vermerkt — `minds fsck` geht dem systematisch nach.
pub fn show_links(
    header: &str,
    links: &[(SessionId, Evidence)],
    store: &dyn minds_store::ContextStore,
    full: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut owned: Vec<(SessionId, Session, Evidence)> = Vec::new();
    let mut orphans = 0usize;
    let mut forgotten: Vec<String> = Vec::new();
    for (id, evidence) in links {
        match store.get(*id) {
            Ok(Some(session)) if !session.intent.request.trim().is_empty() => {
                owned.push((*id, session, *evidence))
            }
            // Ohne Absicht kein Eintrag — stumm weglassen.
            Ok(Some(_)) => {}
            Ok(None) => orphans += 1,
            // Vergessen (DSGVO): die Referenz löst auf, aber auf einen Tombstone.
            // Kein Fehler — der Verweis wird als „vergessen" ausgewiesen.
            Err(minds_store::StoreError::Forgotten { reason, .. }) => forgotten.push(reason),
            Err(err) => return Err(err.into()),
        }
    }

    if owned.is_empty() {
        println!("{}", bold(header));
        for reason in &forgotten {
            println!("   {}", dim(&format!("vergessen ({reason})")));
        }
        if forgotten.is_empty() {
            let note = if orphans > 0 {
                format!("{orphans} Verweis(e) ins Leere — siehe minds fsck")
            } else {
                "kein Minds-Kontext für diesen Commit".to_string()
            };
            println!("   {}", dim(&note));
        }
        return Ok(());
    }

    let shown: Vec<Shown> = owned
        .iter()
        .map(|(id, session, evidence)| Shown {
            id: *id,
            session,
            evidence: *evidence,
        })
        .collect();
    tree(header, &shown, full);

    if orphans > 0 {
        println!("   {}", dim(&format!("+ {orphans} Verweis(e) ins Leere")));
    }
    for reason in &forgotten {
        println!("   {}", dim(&format!("+ vergessen ({reason})")));
    }
    Ok(())
}

/// `true`, wenn stdout ein Terminal ist — nur dann lohnen ANSI-Codes. In eine
/// Pipe oder Datei geht die Ausgabe schmucklos.
fn styled() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Gedämpft (grau) — für Sekundäres. Ohne Terminal unverändert.
fn dim(s: &str) -> String {
    if styled() {
        format!("\u{1b}[2m{s}\u{1b}[0m")
    } else {
        s.to_string()
    }
}

/// Hervorgehoben (fett) — für den Intent und die Überschrift.
fn bold(s: &str) -> String {
    if styled() {
        format!("\u{1b}[1m{s}\u{1b}[0m")
    } else {
        s.to_string()
    }
}

/// Die gekürzte Textform einer Id (`b3-` plus die ersten zwölf Hex-Zeichen),
/// oder die volle bei `full`.
fn short_id(id: SessionId, full: bool) -> String {
    let s = id.to_string();
    if full || s.len() <= 15 {
        s
    } else {
        format!("{}…", &s[..15])
    }
}

/// Die erste sinnvolle Zeile eines Prompts, auf `max` Zeichen an einer
/// Wortgrenze gekürzt — dieselbe deterministische Verdichtung wie der Reader.
fn headline(request: &str, max: usize) -> String {
    minds_reader::summary::headline(request, max)
}

fn edge_kind(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::SpawnedBy => "spawned-by",
        EdgeKind::Spawned => "spawned",
        EdgeKind::ContinuedFrom => "continued-from",
        EdgeKind::Produced => "produced",
    }
}

fn evidence(evidence: Evidence) -> &'static str {
    match evidence {
        Evidence::Observed => "beobachtet",
        Evidence::Content => "inhaltlich",
        Evidence::Declared => "erklärt",
        Evidence::Inferred => "vermutet",
    }
}

fn endpoint(endpoint: &Endpoint) -> String {
    match endpoint {
        Endpoint::Session { agent, local_id } => format!("{agent}/{local_id}"),
        Endpoint::Commit { id } => format!("commit {id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    #[test]
    fn a_trailer_wins_over_an_index_link_for_the_same_session() {
        let trailers = vec![sid('a')];
        let index = vec![
            IndexLink {
                session: sid('a'),
                evidence: Evidence::Inferred,
            },
            IndexLink {
                session: sid('b'),
                evidence: Evidence::Inferred,
            },
        ];
        let merged = merge_links(&trailers, &index);
        assert_eq!(merged.len(), 2);
        // 'a' bleibt beobachtet (Trailer), 'b' kommt als vermutet dazu.
        assert_eq!(merged[0], (sid('a'), Evidence::Observed));
        assert_eq!(merged[1], (sid('b'), Evidence::Inferred));
    }

    #[test]
    fn without_trailers_only_the_index_links_remain() {
        let merged = merge_links(
            &[],
            &[IndexLink {
                session: sid('c'),
                evidence: Evidence::Inferred,
            }],
        );
        assert_eq!(merged, vec![(sid('c'), Evidence::Inferred)]);
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(merge_links(&[], &[]).is_empty());
    }
}
