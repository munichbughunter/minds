//! Eine Session für das Terminal — geteilt von [`crate::show`] und
//! [`crate::why`].
//!
//! Beide Kommandos enden an derselben Stelle: eine [`Session`] liegt vor, sie
//! soll lesbar auf stdout. Damit `show` und `why` nicht zwei Schreibweisen
//! desselben pflegen, steht das Rendern hier — einmal.
//!
//! # Was hier entschärft wird — und was nicht
//!
//! Fast alles, was diese Schicht druckt, ist **fremder Text aus dem Store**:
//! der Prompt, Constraints und verworfene Pfade, die Namen von Agent und
//! Modell, die Dateipfade, die Endpunkte der Kanten, der Grund eines
//! Vergessens. Nichts davon ist beim Anlegen auf Terminal-Sicherheit geprüft
//! worden — die Kanten-Endpunkte liest `edges.rs` sogar wörtlich aus dem
//! Hook-Payload der *Gegenseite*, und die Redaktion aus #35 sucht Geheimnisse,
//! keine Steuerzeichen (#116). Eine ANSI-Sequenz darin löscht Zeilen im
//! Terminal des Lesers, ein Bidi-Zeichen dreht die Leserichtung, ein
//! Zero-Width-Zeichen versteckt Text.
//!
//! Deshalb gilt hier dieselbe Regel wie in [`crate::hooklog`]: **entschärft
//! wird an der Senke**, in genau den Funktionen, die drucken — nicht bei den
//! Aufrufern, und nicht beim Anlegen. Jeder fremde Wert geht durch
//! [`crate::text::sanitize`] — Kennungen wie Agent, Modell, Endpunkte, der
//! Vergessen-Grund. Pfade gehen durch [`crate::text::sanitize_path`] (der
//! Backslash ist dort ein Trenner), und **Prosa** — Prompt, Constraints,
//! Verworfenes — ebenfalls, siehe [`prose`]: Ein Mensch liest sie, niemand
//! parst sie zurück, und die Verdopplung des Backslashs wäre in Code und
//! Windows-Pfaden nur Lärm. Das schließt den `header` ein, den `show` und `why`
//! mitbringen — er trägt die Change-Id aus der Commit-Message beziehungsweise
//! den Pfad aus dem Aufruf.
//!
//! **Bewusst nicht** entschärft: die eigenen Konstanten dieser Schicht
//! (Ast-Zeichen, Labels wie `spawned-by`, die ANSI-Codes von [`dim`] und
//! [`bold`]) und die [`SessionId`], deren Textform beim Parsen auf Hex geprüft
//! ist. Und eine Ausnahme in der *Form*: Der volle Prompt (`--full`) behält
//! seine Zeilen — ein `\n` ist dort Inhalt, kein Angriff. Jede Zeile wird
//! einzeln entschärft und unter dem Ast mit `»` als Zitat eingerückt, sodass
//! der Baum auch bei einem mehrzeiligen Prompt ein Baum bleibt — und eine
//! Prompt-Zeile, die wie ein Ast oder eine Kante *aussieht*, als Zitat
//! erkennbar ist. Alles andere, was wie ein Zeilenumbruch aussieht (`\r`
//! mitten in der Zeile, `U+2028`), wird sichtbar gemacht.

use minds_core::{EdgeKind, Endpoint, Evidence, Session, SessionId};
use minds_store::IndexLink;

use crate::text::{sanitize, sanitize_path};

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
    println!("{}", bold(&sanitize_path(header)));

    let n = items.len();
    for (i, item) in items.iter().enumerate() {
        let last = i + 1 == n;
        let branch = if last { "╰─" } else { "├─" };
        let cont = if last { "   " } else { "│  " };
        let s = item.session;

        // Der Intent ist der Blickfang. Im vollen Modus zeilenweise: die erste
        // Zeile am Ast, die weiteren darunter eingerückt (siehe Modul-Doku).
        let lines = if full {
            prompt_lines(&s.intent.request)
        } else {
            vec![prose(&headline(&s.intent.request, 96))]
        };
        let (first, rest) = lines
            .split_first()
            .map_or(("", &[][..]), |(f, r)| (f.as_str(), r));
        println!("{branch} {}", bold(&format!("▸ {first}")));
        // Mit Zitat-Marker: Eine Prompt-Zeile darf `╰─ ▸ …` oder `Kante: …`
        // heißen — `sanitize` lässt das zu Recht stehen, und ohne Marker läse
        // sich die Zeile wie ein eigener Knoten des Baums.
        for line in rest {
            println!("{cont}  » {}", bold(line));
        }

        // Herkunft, dezent.
        let files = s.produced.files.len();
        let fileword = if files == 1 { "Datei" } else { "Dateien" };
        println!(
            "{cont}{}",
            dim(&format!(
                "{} {} · {}/{} · {}/{} Token · {files} {fileword}",
                sanitize(&s.agent.name),
                sanitize(&s.agent.version),
                sanitize(&s.model.provider),
                sanitize(&s.model.id),
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
                println!("{cont}  Constraint: {}", prose(constraint));
            }
            for discarded in &s.intent.discarded {
                println!("{cont}  Verworfen:  {}", prose(discarded));
            }
            for file in &s.produced.files {
                println!("{cont}  {}", sanitize_path(file));
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
        println!("{}", bold(&sanitize_path(header)));
        for reason in &forgotten {
            println!("   {}", dim(&format!("vergessen ({})", sanitize(reason))));
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
        println!("   {}", dim(&format!("+ vergessen ({})", sanitize(reason))));
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

/// Der volle Prompt, zeilenweise entschärft.
///
/// Geteilt wird **nur** am `\n` (ein `\r` davor gehört zum Zeilenende — ein
/// CRLF-Prompt soll nicht an jeder Zeile ein `\r` zeigen). Alles andere, was
/// eine Zeile vortäuschen könnte, bleibt in [`prose`] und wird sichtbar: ein
/// `\r` mitten in der Zeile als `\r`, `U+2028` als `\u{2028}`. Leere Zeilen am
/// Ende fallen weg, ein leerer Prompt wird zu einer leeren Zeile — der Ast
/// braucht immer eine erste.
fn prompt_lines(request: &str) -> Vec<String> {
    let mut lines: Vec<String> = request
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .map(prose)
        .collect();
    while lines.len() > 1 && lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

/// Text, den ein Mensch geschrieben hat — Prompt, Constraints, verworfene
/// Pfade —, entschärft.
///
/// Ohne die Backslash-Verdopplung von [`sanitize`]: Die braucht `hook.log`, weil
/// dort eine Zeile *eindeutig* rückführbar sein muss. Ein Prompt wird gelesen,
/// nicht zurückgeparst, und er enthält oft Windows-Pfade, Regexe oder Code —
/// `C:\\foo` und `\\d+` wären nur Lärm. Die Steuerzeichen entschärft
/// [`sanitize_path`] genauso; der Backslash ist das einzige, was es durchlässt.
fn prose(text: &str) -> String {
    sanitize_path(text)
}

/// Ein Kanten-Endpunkt, entschärft.
///
/// Der `Session`-Fall ist der fremdeste Text dieser Schicht: `agent` und
/// `local_id` stammen wörtlich aus dem Hook-Payload der Gegenseite und haben
/// nie `SessionKey::new` gesehen (#116). Der Commit-Hash ist beim Anlegen
/// validiert — entschärft wird er trotzdem, damit diese Funktion keine Ausnahme
/// hat, die beim nächsten Endpunkt-Typ vergessen wird.
fn endpoint(endpoint: &Endpoint) -> String {
    match endpoint {
        Endpoint::Session { agent, local_id } => {
            format!("{}/{}", sanitize(agent), sanitize(local_id))
        }
        Endpoint::Commit { id } => format!("commit {}", sanitize(id)),
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

    /// Was ein fremder Hook-Payload in einen Endpunkt legen kann: eine
    /// ANSI-Sequenz, die die Zeile löscht, ein Bidi-Override, ein
    /// Zero-Width-Zeichen und ein Unicode-Tag.
    const HOSTILE: &str = "claude\u{1b}[2K\u{202e}\u{200b}\u{e0041}";

    #[test]
    fn a_hostile_edge_endpoint_does_not_reach_the_terminal_unchanged() {
        // Akzeptanzkriterium aus #116: Der Endpunkt kommt wörtlich aus
        // `edges[].to` des Envelopes, ohne je `SessionKey::new` gesehen zu
        // haben. Was die Redaktion durchlässt, muss hier sichtbar werden.
        let shown = endpoint(&Endpoint::Session {
            agent: HOSTILE.to_string(),
            local_id: format!("run-1{HOSTILE}"),
        });

        for raw in ['\u{1b}', '\u{202e}', '\u{200b}', '\u{e0041}'] {
            assert!(!shown.contains(raw), "{raw:?} roh in {shown:?}");
        }
        assert!(shown.starts_with("claude\\u{1b}[2K"), "{shown:?}");
        assert!(shown.contains("/run-1"), "{shown:?}");
        // Und eine Zeile bleibt eine Zeile.
        assert_eq!(shown.lines().count(), 1, "{shown:?}");
    }

    #[test]
    fn a_commit_endpoint_is_not_exempt() {
        let shown = endpoint(&Endpoint::Commit {
            id: format!("abc123{HOSTILE}"),
        });
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
        assert!(shown.starts_with("commit abc123"), "{shown:?}");
    }

    #[test]
    fn the_full_prompt_keeps_its_lines_but_nothing_else_that_looks_like_one() {
        let lines = prompt_lines("erste\nzweite\r\u{2028}dritte\u{1b}[1m\n\n");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], "erste");
        // `\r`, `U+2028` und ESC werden sichtbar; nur `\n` trennt.
        assert_eq!(lines[1], "zweite\\r\\u{2028}dritte\\u{1b}[1m");
    }

    #[test]
    fn a_crlf_prompt_reads_like_a_lf_prompt() {
        // Ein Windows-Agent oder kopierter Text: Das `\r` vor dem `\n` ist
        // Zeilenende, kein Inhalt — und darf nicht an jeder Zeile kleben.
        assert_eq!(prompt_lines("a\r\nb\r\n"), vec!["a", "b"]);
    }

    #[test]
    fn prose_keeps_its_backslashes_but_nothing_dangerous() {
        assert_eq!(prose("C:\\foo und \\d+"), "C:\\foo und \\d+");
        assert_eq!(prose("x\u{1b}[2K\u{202e}y"), "x\\u{1b}[2K\\u{202e}y");
    }

    #[test]
    fn an_empty_prompt_still_yields_a_first_line() {
        assert_eq!(prompt_lines(""), vec![String::new()]);
        assert_eq!(prompt_lines("\n\n"), vec![String::new()]);
    }
}
