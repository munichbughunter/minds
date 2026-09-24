//! `minds blame <datei>` — welcher Agent, welche Session steckt hinter welchen
//! Zeilen einer Datei.
//!
//! Die git-vertraute Frage („wer war das?"), aber bis zur Session
//! weitergedacht: pro Zeile das Blame → Commit → Session, dann nach Session
//! aggregiert. `why` beantwortet *eine* Zeile im Detail, `blame` gibt den
//! Überblick über die *ganze* Datei — inklusive der ehrlichen Zahl, wie viel
//! davon überhaupt erfassten Kontext hat.
//!
//! Zwei Ansichten auf **dieselbe** Zuordnung: ohne Flag die Aggregation nach
//! Session, mit `--lines` eine Zeile Ausgabe je Quellzeile — die von
//! `git blame` vertraute Form, in der man eine Datei von oben nach unten
//! liest. Beide Ansichten teilen sich [`attribute`]; welche Session eine Zeile
//! „besitzt", entscheidet also nie die Ansicht.
//!
//! Ein Unterschied ist Absicht: `--lines` druckt **keine** Schlusszeile
//! „N line(s) without captured context". Was ohne Kontext ist, steht dort
//! schon in jeder betroffenen Zeile als `-`; die Wiederholung am Ende wäre
//! Rauschen. Das ist kein Versehen und will nicht „repariert" werden.
//!
//! Geblamed wird **HEAD**, nicht der Arbeitsstand (siehe `minds-git::blame`):
//! eine uncommittete Änderung darf die Zeilennummern nicht verschieben.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use minds_core::{Session, SessionId};
use minds_git::{BlameLine, BlameProvider, CommitId};

use crate::context::{Context, Skipped};

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Was in der `--lines`-Ansicht dort steht, wo keine Session steht.
const NOTHING: &str = "-";

/// Der Abstand zwischen zwei Spalten der `--lines`-Ansicht.
const GUTTER: &str = "  ";

/// Führt `minds blame` aus. `target` ist ein repo-relativer Dateipfad.
///
/// `per_line` ist `--lines`: eine Zeile Ausgabe je Quellzeile statt der
/// Aggregation nach Session.
pub fn run(target: Option<&str>, per_line: bool) -> ExitCode {
    let Some(path) = target else {
        eprintln!("minds blame: expected <file>");
        return ExitCode::FAILURE;
    };
    match blame(path, per_line) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds blame: {err}");
            ExitCode::FAILURE
        }
    }
}

fn blame(path: &str, per_line: bool) -> Fallible<()> {
    let ctx = Context::open()?;
    let Some(head) = ctx.repo.head()?.commit() else {
        return Err("HEAD has no commit yet".into());
    };

    let lines = ctx.repo.blame().blame_file(head, path)?;
    if lines.is_empty() {
        return Err(format!("{path} cannot be resolved in blame (not in the commit?)").into());
    }
    let total = lines.len();

    let attribution = attribute(&ctx, &lines)?;
    if let Some(note) = attribution.skipped.note() {
        eprintln!("minds blame: {note}");
    }

    let with_context = total as u32 - attribution.without;
    let pct = with_context as usize * 100 / total;
    println!("{path} — {total} lines, {with_context} with captured context ({pct}%)\n");

    let view = if per_line {
        // Der Quelltext kommt aus demselben Commit wie das Blame — nicht aus
        // dem Arbeitsstand, sonst stünde neben der Zeilennummer aus HEAD ein
        // Text, den HEAD nie hatte. Dass die Datei dort existiert, hat das
        // Blame oben bereits bewiesen; `unwrap_or_default` hält den Fall
        // trotzdem aus, statt sich auf diesen Beweis zu verlassen.
        let tree = ctx.repo.tree_of(head)?;
        let content = ctx.repo.read_blob(tree, path)?.unwrap_or_default();
        per_line_view(&attribution, &content)
    } else {
        summary_view(&attribution)
    };
    print!("{view}");
    Ok(())
}

/// Eine geblamete Zeile und die Session, der sie zugeschrieben wird.
struct Attributed {
    /// Die Zeilennummer aus dem Blame — 1-basiert.
    line: u32,
    session: Option<SessionId>,
}

/// Das Ergebnis der Zuordnung Zeile → Commit → Session, aus dem beide
/// Ansichten entstehen.
struct Attribution {
    lines: Vec<Attributed>,
    session_of: BTreeMap<SessionId, Session>,
    /// Zeilen ohne erfassten Kontext — die Gegenzahl zur Deckung im Kopf.
    without: u32,
    skipped: Skipped,
}

/// Ordnet jede geblamete Zeile ihrer Session zu (über den Commit).
///
/// Commit→Sessions wird gecacht, damit eine 1000-Zeilen-Datei nicht 1000
/// Store-Lookups auslöst.
fn attribute(ctx: &Context, lines: &[BlameLine]) -> Fallible<Attribution> {
    let mut attribution = Attribution {
        lines: Vec::with_capacity(lines.len()),
        session_of: BTreeMap::new(),
        without: 0,
        skipped: Skipped::default(),
    };
    let mut commit_cache: BTreeMap<CommitId, Vec<SessionId>> = BTreeMap::new();

    for entry in lines {
        let ids = match commit_cache.get(&entry.commit) {
            Some(ids) => ids.clone(),
            None => {
                let (linked, s) = ctx.linked_sessions(entry.commit)?;
                attribution.skipped.merge(s);
                let ids: Vec<SessionId> = linked
                    .into_iter()
                    .filter(|(_, session)| !session.intent.request.trim().is_empty())
                    .map(|(id, session)| {
                        attribution.session_of.entry(id).or_insert(session);
                        id
                    })
                    .collect();
                commit_cache.insert(entry.commit, ids.clone());
                ids
            }
        };
        // Mehrere Sessions am selben Commit: die Zeile der ersten zuschreiben,
        // damit die Summe der Zeilen die Dateigröße nicht übersteigt.
        let session = ids.first().copied();
        if session.is_none() {
            attribution.without += 1;
        }
        attribution.lines.push(Attributed {
            line: entry.line,
            session,
        });
    }

    Ok(attribution)
}

/// Die Vorgabe-Ansicht: nach Session aggregiert, die stärkste zuerst.
fn summary_view(attribution: &Attribution) -> String {
    let mut lines_per_session: BTreeMap<SessionId, u32> = BTreeMap::new();
    for id in attribution.lines.iter().filter_map(|line| line.session) {
        *lines_per_session.entry(id).or_default() += 1;
    }

    let mut ranked: Vec<(SessionId, u32)> = lines_per_session.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut out = String::new();
    for (id, count) in &ranked {
        let session = &attribution.session_of[id];
        let headline = minds_reader::summary::headline(&session.intent.request, 70);
        let _ = writeln!(out, "▸ {headline}");
        let _ = writeln!(
            out,
            "  {count} line(s) · {} · {} · {}",
            session.agent.name,
            session.model.id,
            short_id(*id),
        );
    }

    if attribution.without > 0 {
        let _ = writeln!(
            out,
            "\n{} line(s) without captured context",
            attribution.without
        );
    }
    out
}

/// Die `--lines`-Ansicht über `content`, dem Dateiinhalt im geblameten Commit.
fn per_line_view(attribution: &Attribution, content: &[u8]) -> String {
    let rows: Vec<Row> = attribution
        .lines
        .iter()
        .map(|line| match line.session {
            Some(id) => Row {
                line: line.line,
                id: short_id(id),
                agent: attribution.session_of[&id].agent.name.clone(),
            },
            None => Row {
                line: line.line,
                id: NOTHING.to_string(),
                agent: NOTHING.to_string(),
            },
        })
        .collect();
    render(&rows, content)
}

/// Eine Zeile der `--lines`-Ansicht, fertig zum Ausrichten: Kurz-Id und
/// Agentname stehen hier schon als Text — `-` für „kein erfasster Kontext"
/// eingeschlossen.
struct Row {
    line: u32,
    id: String,
    agent: String,
}

/// Setzt die Zeilen, jede Spalte so breit wie ihr breitester Wert.
///
/// Die Breiten entstehen aus *dieser* Ausgabe, nicht aus einer festen Vorgabe:
/// So steht in einer Datei mit einer einzigen Session kein leeres Feld, und in
/// einer mit fünf Sessions rutscht nichts.
///
/// Der Quelltext wird **nicht** angetastet — kein Kürzen, kein Umbrechen, kein
/// Ersetzen. Nur nach UTF-8 wird tolerant gelesen (`from_utf8_lossy`): Eine
/// Binärdatei darf krude aussehen, aber nicht abstürzen.
///
/// Die Funktion rechnet ausschließlich mit dem, was sie bekommt: Die Breite der
/// Zeilennummern kommt aus der größten Nummer, die hier gedruckt wird, nicht aus
/// der Anzahl der Zeilen — beides ist heute dasselbe, aber nur das erste bleibt
/// richtig, wenn je ein Ausschnitt einer Datei hier landet.
fn render(rows: &[Row], content: &[u8]) -> String {
    let source = source_lines(content);
    let id_width = width_of(rows.iter().map(|row| row.id.as_str()));
    let agent_width = width_of(rows.iter().map(|row| row.agent.as_str()));
    let line_width = largest_line(rows).to_string().len();

    let mut out = String::new();
    for row in rows {
        let text = (row.line as usize)
            .checked_sub(1)
            .and_then(|index| source.get(index))
            .map(|bytes| String::from_utf8_lossy(bytes))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{:id_width$}{GUTTER}{:agent_width$}{GUTTER}{:>line_width$}{GUTTER}{text}",
            row.id, row.agent, row.line,
        );
    }
    out
}

/// Die Breite der breitesten Spaltenzelle — in Zeichen, nicht in Bytes, damit
/// das `…` der gekürzten Session-Id nicht drei Stellen belegt.
fn width_of<'a>(values: impl Iterator<Item = &'a str>) -> usize {
    values.map(|value| value.chars().count()).max().unwrap_or(0)
}

/// Die größte Zeilennummer der Ausgabe — sie bestimmt, wie breit die Spalte wird.
fn largest_line(rows: &[Row]) -> u32 {
    rows.iter().map(|row| row.line).max().unwrap_or(0)
}

/// Zerlegt den Dateiinhalt so in Zeilen, wie `git blame` sie nummeriert: Die
/// letzte Zeile zählt auch dann, wenn kein Zeilenumbruch mehr folgt — und ein
/// abschließender Umbruch eröffnet keine leere letzte Zeile.
///
/// Dieselbe Regel wie `line_count` in `minds-git::blame`; weichen beide
/// voneinander ab, stünde neben einer Zeilennummer der falsche Text.
fn source_lines(content: &[u8]) -> Vec<&[u8]> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = content.split(|byte| *byte == b'\n').collect();
    if content.last() == Some(&b'\n') {
        lines.pop();
    }
    lines
}

/// `b3-` plus die ersten zwölf Hex-Zeichen — genug, um Sessions zu unterscheiden.
fn short_id(id: SessionId) -> String {
    let s = id.to_string();
    if s.len() <= 15 {
        s
    } else {
        format!("{}…", &s[..15])
    }
}

#[cfg(test)]
mod tests {
    use minds_core::{Agent, Intent, Model};

    use super::*;

    fn row(line: u32, id: &str, agent: &str) -> Row {
        Row {
            line,
            id: id.to_string(),
            agent: agent.to_string(),
        }
    }

    fn nothing(line: u32) -> Row {
        row(line, NOTHING, NOTHING)
    }

    fn session(agent: &str, model: &str, request: &str) -> Session {
        Session::new(
            Agent {
                name: agent.into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: model.into(),
            },
            Intent {
                request: request.into(),
                ..Default::default()
            },
        )
    }

    /// Die Vorgabe-Ansicht ist der Stand **vor** `--lines` und muss es
    /// bleiben: Rumpfzeile, Rangfolge (mehr Zeilen zuerst) und Schlusszeile
    /// stehen hier wortgetreu. Der Umbau auf zwei Ansichten hat genau diese
    /// Formatierung angefasst — ein verlorenes `·` oder ein vertauschtes
    /// Agent/Modell bliebe sonst grün.
    #[test]
    fn the_summary_view_keeps_its_shape() {
        let busy = session("claude-code", "opus-5", "Backoff einbauen");
        let quiet = session("codex", "gpt-5", "Test reparieren");
        let busy_id = SessionId::of(&busy).expect("Session kanonisiert");
        let quiet_id = SessionId::of(&quiet).expect("Session kanonisiert");

        let attribution = Attribution {
            lines: vec![
                Attributed {
                    line: 1,
                    session: Some(busy_id),
                },
                Attributed {
                    line: 2,
                    session: Some(quiet_id),
                },
                Attributed {
                    line: 3,
                    session: Some(busy_id),
                },
                Attributed {
                    line: 4,
                    session: None,
                },
            ],
            session_of: BTreeMap::from([(busy_id, busy), (quiet_id, quiet)]),
            without: 1,
            skipped: Skipped::default(),
        };

        assert_eq!(
            summary_view(&attribution),
            format!(
                "\
▸ Backoff einbauen
  2 line(s) · claude-code · opus-5 · {}
▸ Test reparieren
  1 line(s) · codex · gpt-5 · {}

1 line(s) without captured context
",
                short_id(busy_id),
                short_id(quiet_id),
            )
        );
    }

    /// Der Normalfall: jede Zeile hat Kontext, die Spalten sind so breit wie
    /// ihr Inhalt.
    #[test]
    fn every_line_names_its_session() {
        let rows = vec![
            row(1, "b3-a1b2c3d4e5f6", "claude-code"),
            row(2, "b3-a1b2c3d4e5f6", "claude-code"),
            row(3, "b3-a1b2c3d4e5f6", "claude-code"),
        ];
        assert_eq!(
            render(&rows, b"eins\nzwei\ndrei\n"),
            "\
b3-a1b2c3d4e5f6  claude-code  1  eins
b3-a1b2c3d4e5f6  claude-code  2  zwei
b3-a1b2c3d4e5f6  claude-code  3  drei
"
        );
    }

    /// Zeilen ohne erfassten Kontext sind sichtbar, nicht weggelassen — und
    /// die Schlusszeile der Aggregation fehlt hier bewusst: Was ohne Kontext
    /// ist, steht in der Zeile selbst.
    #[test]
    fn a_line_without_context_shows_a_dash_and_no_footer() {
        let rows = vec![
            nothing(1),
            row(2, "b3-a1b2c3d4e5f6", "claude-code"),
            nothing(3),
        ];
        let out = render(&rows, b"eins\nZWEI\ndrei\n");
        assert_eq!(
            out,
            "\
-                -            1  eins
b3-a1b2c3d4e5f6  claude-code  2  ZWEI
-                -            3  drei
"
        );
        assert!(!out.contains("without captured context"), "{out}");
    }

    /// Die Spalten richten sich am breitesten Wert aus — sonst rutscht die
    /// Ansicht, sobald zwei Sessions unterschiedlich lange Agentnamen haben.
    #[test]
    fn the_columns_align_to_the_widest_value() {
        let rows = vec![
            row(1, "b3-aaaaaaaaaaaa…", "claude-code"),
            row(2, "b3-bbb", "codex"),
        ];
        assert_eq!(
            render(&rows, b"eins\nzwei\n"),
            "\
b3-aaaaaaaaaaaa…  claude-code  1  eins
b3-bbb            codex        2  zwei
"
        );
    }

    /// Die Zeilennummer ist rechtsbündig und so breit wie die letzte Zeile —
    /// bei zweistelligen Dateien rutscht die einstellige Zahl nach rechts.
    #[test]
    fn the_line_number_is_right_aligned_to_the_file_length() {
        let rows: Vec<Row> = (1..=10).map(|n| row(n, "b3-a", "claude-code")).collect();
        let content: String = (1..=10).map(|n| format!("Zeile {n}\n")).collect();
        let out = render(&rows, content.as_bytes());
        assert!(out.starts_with("b3-a  claude-code   1  Zeile 1\n"), "{out}");
        assert!(out.ends_with("b3-a  claude-code  10  Zeile 10\n"), "{out}");
    }

    /// Die Breite kommt aus der größten **Nummer**, nicht aus der Anzahl der
    /// Zeilen: Wer je einen Ausschnitt hier hindurchschickt, soll keine
    /// verrutschte Spalte bekommen.
    #[test]
    fn the_line_number_column_follows_the_largest_number() {
        let rows = vec![
            row(9, "b3-a", "claude-code"),
            row(10, "b3-a", "claude-code"),
        ];
        assert_eq!(
            render(
                &rows,
                b"eins\nzwei\ndrei\nvier\nfuenf\nsechs\nsieben\nacht\nneun\nzehn\n"
            ),
            "\
b3-a  claude-code   9  neun
b3-a  claude-code  10  zehn
"
        );
    }

    /// Eine leere Quellzeile bleibt eine Zeile — mit ihren Spalten davor. Dass
    /// dabei zwei Leerzeichen am Ende stehen, ist die Folge der festen
    /// Spaltenform und hier bewusst festgeschrieben, nicht zufällig.
    #[test]
    fn an_empty_source_line_keeps_its_columns() {
        let rows = vec![
            row(1, "b3-a", "claude-code"),
            row(2, "b3-a", "claude-code"),
            row(3, "b3-a", "claude-code"),
        ];
        assert_eq!(
            render(&rows, b"eins\n\ndrei\n"),
            "\
b3-a  claude-code  1  eins
b3-a  claude-code  2  
b3-a  claude-code  3  drei
"
        );
    }

    /// Laufen Blame und Blob je auseinander, fehlt der Text — nicht die Zeile,
    /// und schon gar nicht die ganze Ausgabe.
    #[test]
    fn a_missing_source_still_yields_one_row_per_line() {
        let rows = vec![row(1, "b3-a", "claude-code"), row(2, "b3-a", "claude-code")];
        assert_eq!(render(&rows, b"").lines().count(), rows.len());
    }

    /// Eine Datei ohne Zeilenumbruch am Ende hat trotzdem eine letzte Zeile —
    /// dieselbe Zählung wie im Blame, sonst stünde dort nichts.
    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_line() {
        let rows = vec![row(1, "b3-a", "claude-code"), row(2, "b3-a", "claude-code")];
        assert_eq!(
            render(&rows, b"eins\nzwei"),
            "\
b3-a  claude-code  1  eins
b3-a  claude-code  2  zwei
"
        );
        assert_eq!(source_lines(b"eins\nzwei").len(), 2);
        assert_eq!(source_lines(b"eins\nzwei\n").len(), 2);
        assert_eq!(source_lines(b"").len(), 0);
        assert_eq!(source_lines(b"\n").len(), 1);
    }

    /// Eine Binärdatei darf krude aussehen — abstürzen darf sie nicht.
    #[test]
    fn invalid_utf8_renders_instead_of_panicking() {
        let rows = vec![row(1, "b3-a", "claude-code")];
        let out = render(&rows, &[0xff, 0xfe, b'\n']);
        assert!(out.starts_with("b3-a  claude-code  1  "), "{out}");
        assert!(out.ends_with('\n'), "{out}");
    }
}
