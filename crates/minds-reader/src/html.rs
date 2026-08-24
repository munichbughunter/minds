//! Das Rendern — reine Funktionen von Daten nach HTML-Text.
//!
//! Kein Template-Modul, keine Abhängigkeit: Die Seite ist klein genug, um sie
//! mit `format!` zu schreiben, und das hält den Reader bei genau den drei
//! Crates, die er ohnehin braucht.
//!
//! # Selbsttragend
//!
//! Jede Seite bringt ihr CSS und ihr JavaScript **inline** mit. Kein CDN, keine
//! Schriftart aus dem Netz, kein Build-Schritt. Damit funktioniert die Ausgabe
//! auch über `file://`, im Air-Gap und hinter jeder Firewall — dieselbe
//! Bedingung, unter der die Kundschaft aus der Vision arbeitet.
//!
//! # Warum die Panels in Rust vorgerendert werden
//!
//! Der naheliegende Weg wäre, die Sessions als JSON einzubetten und im Browser
//! zu einem Panel zu bauen. Das hieße: ein zweites Datenformat, ein
//! Template-Dialekt in JavaScript und eine Abhängigkeit auf `serde_json`.
//! Stattdessen wird jedes Session-Panel **einmal in Rust** gerendert und fertig
//! in die Seite gelegt; das Skript tut nur noch zweierlei — beim Laden alle
//! verstecken und auf Klick das richtige zeigen.
//!
//! Die Reihenfolge ist Absicht: Ausgeliefert wird **sichtbar**, versteckt wird
//! erst im Browser. Ohne JavaScript steht damit jede Session lesbar unter dem
//! Code, statt dass die Seite leer wirkt — progressive Verbesserung, keine
//! Voraussetzung.

use std::collections::BTreeMap;

use minds_core::{EffectKind, Role, Session, SessionId, ToolCall};
use minds_git::{CommitDiff, DiffFile, DiffKind, DiffLine};

use crate::file::FileView;
use crate::index::Index;
use crate::summary::Summary;

/// Eine Datei in der Übersicht: wohin sie zeigt und wie viel Kontext sie trägt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLink {
    /// Repo-relativer Pfad, wie er angezeigt wird.
    pub path: String,
    /// Dateiname der erzeugten Seite, relativ zur Übersicht.
    pub href: String,
    /// Zeilen mit erfasstem Kontext.
    pub attributed: usize,
    /// Zeilen insgesamt.
    pub total: usize,
}

/// Baut ein vollständiges HTML-Dokument um `body`.
pub fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"de\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>{STYLE}</style>\n\
         </head>\n\
         <body>\n{body}\n<script>{SCRIPT}</script>\n</body>\n</html>\n",
        title = escape(title),
    )
}

/// Die Seite einer Datei: links der Code mit klickbaren Zeilen, rechts die
/// Session hinter der angeklickten Zeile.
pub fn file_page(view: &FileView, index: &Index) -> String {
    let attributed = view.attributed_lines();
    let total = view.lines.len();
    let pct = (attributed * 100).checked_div(total).unwrap_or(0);
    let body = format!(
        "<header class=\"top\">\n\
           <a class=\"back\" href=\"index.html\">← Übersicht</a>\n\
           <h1>{path}</h1>\n\
           <div class=\"attrbar\" title=\"{pct}% der Zeilen mit erfasstem Agent-Kontext\">\
             <div class=\"attrbar-fill\" style=\"width:{pct}%\"></div></div>\n\
           <p class=\"meta\">{attributed} von {total} Zeilen mit erfasstem Kontext · {pct}% Agent</p>\n\
         </header>\n\
         <main class=\"split\">\n\
           <div class=\"code\">{code}</div>\n\
           <aside class=\"panel\" id=\"panel\">\n\
             <p class=\"hint\">Klick auf eine markierte Zeile zeigt die Session dahinter.</p>\n\
             {panels}\n\
           </aside>\n\
         </main>\n",
        path = escape(&view.path),
        code = code_block(view),
        panels = panels_for(view, index),
    );
    page(&view.path, &body)
}

/// Der Codeblock: je Zeile eine Zeilennummer und der Text. Zeilen mit Kontext
/// tragen `data-sessions` und werden dadurch klickbar. Der Text läuft durch das
/// Highlighting (U.7) — das jedes Token **einzeln escaped**, die XSS-Zusage bleibt.
fn code_block(view: &FileView) -> String {
    let lang = lang_of(&view.path);
    let mut out = String::new();
    for line in &view.lines {
        let ids = line
            .sessions
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(" ");

        if line.is_attributed() {
            let count = line.sessions.len();
            let title = if count == 1 {
                "Session hinter dieser Zeile anzeigen".to_string()
            } else {
                format!("{count} Sessions hinter dieser Zeile anzeigen")
            };
            out.push_str(&format!(
                "<div class=\"line has-context\" data-sessions=\"{ids}\" tabindex=\"0\" \
                 role=\"button\" title=\"{title}\">\
                 <span class=\"num\">{num}</span><code>{text}</code></div>\n",
                ids = escape(&ids),
                title = escape(&title),
                num = line.number,
                text = highlight(&line.text, lang.as_ref()),
            ));
        } else {
            out.push_str(&format!(
                "<div class=\"line\"><span class=\"num\">{num}</span><code>{text}</code></div>\n",
                num = line.number,
                text = highlight(&line.text, lang.as_ref()),
            ));
        }
    }
    out
}

/// Ein Sprachprofil fürs Highlighting: Schlüsselwörter und Zeilenkommentar.
struct Lang {
    keywords: &'static [&'static str],
    line_comment: &'static str,
}

/// Rät die Sprache aus der Dateiendung. `None` = kein Highlighting (nur escapen).
fn lang_of(path: &str) -> Option<Lang> {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => Some(Lang {
            keywords: RUST_KW,
            line_comment: "//",
        }),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => Some(Lang {
            keywords: JS_KW,
            line_comment: "//",
        }),
        "py" => Some(Lang {
            keywords: PY_KW,
            line_comment: "#",
        }),
        _ => None,
    }
}

const RUST_KW: &[&str] = &[
    "fn", "let", "mut", "const", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for",
    "while", "loop", "if", "else", "match", "return", "self", "Self", "as", "ref", "in", "where",
    "dyn", "move", "async", "await", "unsafe", "crate", "super", "type", "static", "true", "false",
];

const JS_KW: &[&str] = &[
    "function",
    "const",
    "let",
    "var",
    "if",
    "else",
    "for",
    "while",
    "return",
    "class",
    "new",
    "this",
    "import",
    "export",
    "from",
    "default",
    "async",
    "await",
    "try",
    "catch",
    "throw",
    "typeof",
    "instanceof",
    "true",
    "false",
    "null",
    "undefined",
];

const PY_KW: &[&str] = &[
    "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as", "try",
    "except", "finally", "with", "lambda", "None", "True", "False", "and", "or", "not", "in", "is",
    "pass", "raise", "yield", "global", "nonlocal", "async", "await",
];

/// Sehr schlankes Highlighting: erkennt Zeilenkommentar, String-Literale, Zahlen
/// und Schlüsselwörter. Jedes ausgegebene Stück geht durch [`escape`] — ein
/// Prompt oder Code mit `<script>` kann so nichts injizieren.
fn highlight(text: &str, lang: Option<&Lang>) -> String {
    let Some(lang) = lang else {
        return escape(text);
    };
    let chars: Vec<char> = text.chars().collect();
    let comment: Vec<char> = lang.line_comment.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        if slice_starts_with(&chars, i, &comment) {
            let rest: String = chars[i..].iter().collect();
            span(&mut out, "hl-com", &rest);
            break;
        }
        let c = chars[i];
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            let mut esc = false;
            while j < chars.len() {
                let cj = chars[j];
                if esc {
                    esc = false;
                } else if cj == '\\' {
                    esc = true;
                } else if cj == c {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let s: String = chars[i..j].iter().collect();
            span(&mut out, "hl-str", &s);
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            let s: String = chars[i..j].iter().collect();
            span(&mut out, "hl-num", &s);
            i = j;
        } else if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if lang.keywords.contains(&word.as_str()) {
                span(&mut out, "hl-kw", &word);
            } else {
                out.push_str(&escape(&word));
            }
            i = j;
        } else {
            out.push_str(&escape(&c.to_string()));
            i += 1;
        }
    }
    out
}

/// Hängt `<span class="cls">escape(text)</span>` an.
fn span(out: &mut String, cls: &str, text: &str) {
    out.push_str(&format!("<span class=\"{cls}\">{}</span>", escape(text)));
}

fn slice_starts_with(chars: &[char], i: usize, pat: &[char]) -> bool {
    !pat.is_empty() && i + pat.len() <= chars.len() && chars[i..i + pat.len()] == *pat
}

/// Alle Panels, die auf dieser Seite gebraucht werden — versteckt, bis eine
/// Zeile sie aufruft.
fn panels_for(view: &FileView, index: &Index) -> String {
    let mut out = String::new();
    for id in view.sessions() {
        match index.session(id) {
            Some(session) => out.push_str(&session_panel(id, session, !index.is_observed(id))),
            // Der Trailer nennt eine Session, die der Store nicht hat.
            None => out.push_str(&format!(
                "<section class=\"session orphan\" id=\"s-{id}\">\n\
                   <h2>{id}</h2>\n\
                   <p class=\"warn\">Diese Session liegt nicht im Store — der Verweis ist verwaist.</p>\n\
                 </section>\n",
                id = escape(&id.to_string()),
            )),
        }
    }
    out
}

/// Ein einzelnes Session-Panel.
///
/// Wird **sichtbar** ausgeliefert; erst das Skript versteckt es beim Laden.
/// Ohne JavaScript steht damit jede Session lesbar unter dem Code, statt dass
/// die Seite leer wirkt — progressive Verbesserung statt Voraussetzung.
pub fn session_panel(id: SessionId, session: &Session, inferred: bool) -> String {
    let badge = if inferred {
        "<p class=\"guess\">⚠ vermutet — heuristisch verknüpft, nicht über einen Trailer belegt</p>\n"
    } else {
        ""
    };
    let mut out = format!(
        "<section class=\"session\" id=\"s-{id}\">\n\
           {badge}\
           <h2>Absicht</h2>\n\
           <p class=\"intent\">{request}</p>\n",
        id = escape(&id.to_string()),
        request = escape(non_empty(&session.intent.request, "(kein Prompt erfasst)")),
    );

    if !session.intent.constraints.is_empty() {
        out.push_str("<h3>Constraints</h3>\n<ul>\n");
        for constraint in &session.intent.constraints {
            out.push_str(&format!("<li>{}</li>\n", escape(constraint)));
        }
        out.push_str("</ul>\n");
    }

    if !session.intent.discarded.is_empty() {
        out.push_str("<h3>Verworfen</h3>\n<ul>\n");
        for discarded in &session.intent.discarded {
            out.push_str(&format!("<li>{}</li>\n", escape(discarded)));
        }
        out.push_str("</ul>\n");
    }

    out.push_str(&format!(
        "<h3>Herkunft</h3>\n\
         <dl>\n\
           <dt>Agent</dt><dd>{agent} {version}</dd>\n\
           <dt>Modell</dt><dd>{provider} / {model}</dd>\n\
           <dt>Tokens</dt><dd>{input} ein / {output} aus</dd>\n\
           <dt>Session</dt><dd><code class=\"id\">{id}</code></dd>\n\
         </dl>\n",
        agent = escape(&session.agent.name),
        version = escape(&session.agent.version),
        provider = escape(&session.model.provider),
        model = escape(&session.model.id),
        input = session.usage.input_tokens,
        output = session.usage.output_tokens,
        id = escape(&id.to_string()),
    ));

    if !session.produced.files.is_empty() {
        out.push_str("<h3>Berührte Dateien</h3>\n<ul>\n");
        for file in &session.produced.files {
            out.push_str(&format!("<li><code>{}</code></li>\n", escape(file)));
        }
        out.push_str("</ul>\n");
    }

    out.push_str(&turns_html(session));

    out.push_str("</section>\n");
    out
}

/// Der Gesprächsverlauf (U.1): je Turn eine Rolle, der Text und — aufklappbar —
/// die Tool-Calls dieses Zugs. Leer, wenn keine Turns erfasst sind (etwa bei
/// importierten Sessions, die nur die Absicht tragen).
fn turns_html(session: &Session) -> String {
    if session.turns.is_empty() {
        return String::new();
    }
    let mut out = String::from("<h3>Verlauf</h3>\n<div class=\"timeline\">\n");
    for turn in &session.turns {
        let (role_cls, role_label) = match turn.role {
            Role::User => ("user", "User"),
            Role::Assistant => ("assistant", "Assistant"),
            Role::System => ("system", "System"),
            Role::Tool => ("tool", "Tool"),
        };
        out.push_str(&format!(
            "<div class=\"turn {role_cls}\"><span class=\"role\">{role_label}</span>"
        ));
        if !turn.text.trim().is_empty() {
            out.push_str(&format!(
                "<div class=\"turn-text\">{}</div>",
                escape(turn.text.trim())
            ));
        }
        if !turn.tool_calls.is_empty() {
            out.push_str(&tool_calls_html(&turn.tool_calls));
        }
        out.push_str("</div>\n");
    }
    out.push_str("</div>\n");
    out
}

/// Die Tool-Calls eines Zugs (U.2), aufklappbar: je Call Name, Effekt-Badge und
/// das Wesentliche — der Pfad bei Datei-Effekten, das entrauschte Kommando bei
/// Exec.
fn tool_calls_html(calls: &[ToolCall]) -> String {
    let n = calls.len();
    let word = if n == 1 { "Tool-Call" } else { "Tool-Calls" };
    let mut out = format!(
        "<details class=\"tools\"><summary>{n} {word}</summary>\n<ul class=\"toollist\">\n"
    );
    for call in calls {
        out.push_str(&format!(
            "<li><span class=\"tool-name\">{}</span>",
            escape(&call.name)
        ));
        if let Some(effect) = &call.effect {
            out.push_str(&format!(
                " <span class=\"effect {cls}\">{label}</span>",
                cls = effect_class(effect.kind),
                label = effect_label(effect.kind),
            ));
        }
        let detail = call_detail(call);
        if !detail.is_empty() {
            out.push_str(&format!(
                " <code class=\"tool-detail\">{}</code>",
                escape(&detail)
            ));
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ul></details>\n");
    out
}

/// Das anzeigenswerte Detail eines Tool-Calls: bei Exec das entrauschte
/// Kommando, sonst der berührte Pfad.
fn call_detail(call: &ToolCall) -> String {
    match &call.effect {
        Some(effect) if effect.kind == EffectKind::Exec => {
            minds_core::extract::command_of(&call.arguments).unwrap_or_default()
        }
        Some(effect) => effect.path.clone().unwrap_or_default(),
        None => String::new(),
    }
}

fn effect_class(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Read => "read",
        EffectKind::Write => "write",
        EffectKind::Delete => "delete",
        EffectKind::Exec => "exec",
        EffectKind::Other => "other",
    }
}

fn effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Read => "read",
        EffectKind::Write => "write",
        EffectKind::Delete => "delete",
        EffectKind::Exec => "exec",
        EffectKind::Other => "tool",
    }
}

/// Die Seite einer **Session**: oben die Absicht (dasselbe Panel wie in der
/// Datei-Ansicht), darunter die **Änderungen** — alle Dateien, die die
/// zugehörigen Commits berührt haben, jede auf- und zuklappbar mit ihren Zeilen.
///
/// Das ist das Ziel aus dem Feedback: Ein Klick auf eine Übersichts-Karte führt
/// hierher und zeigt *alle* betroffenen Dateien samt Änderungen, nicht nur die
/// erste. `file_href` verlinkt jede geänderte Datei — sofern der Reader eine
/// Seite für sie gebaut hat — auf ihre zeilenweise Ansicht.
pub fn session_page(
    id: SessionId,
    session: &Session,
    diffs: &[CommitDiff],
    inferred: bool,
    file_href: &BTreeMap<String, String>,
) -> String {
    let summary = Summary::of(id, session);
    let changed: usize = diffs.iter().map(|d| d.files.len()).sum();
    let fileword = if changed == 1 { "Datei" } else { "Dateien" };
    let body = format!(
        "<header class=\"top\">\n\
           <a class=\"back\" href=\"index.html\">← Übersicht</a>\n\
           <h1>{headline}</h1>\n\
           <p class=\"meta\">{actor} · {changed} {fileword} geändert</p>\n\
         </header>\n\
         <main class=\"session-view\">\n\
           {panel}\
           <h2 class=\"changes-h\">Änderungen</h2>\n\
           {changes}\n\
         </main>\n",
        headline = escape(&summary.headline),
        actor = escape(&summary.actor),
        panel = session_panel(id, session, inferred),
        changes = changes_html(diffs, file_href),
    );
    page(&summary.headline, &body)
}

/// Alle Datei-Änderungen einer Session, jede in einem aufklappbaren Block.
fn changes_html(diffs: &[CommitDiff], file_href: &BTreeMap<String, String>) -> String {
    let total: usize = diffs.iter().map(|d| d.files.len()).sum();
    if total == 0 {
        return "<p class=\"empty\">Keine dem Commit zugeordnete Änderung — \
                die Session ist erfasst, aber (noch) nicht mit einem Commit verbunden.</p>\n"
            .to_string();
    }
    let mut out = String::new();
    for diff in diffs {
        for file in &diff.files {
            out.push_str(&diff_file_html(file, file_href));
        }
    }
    out
}

/// Ein aufklappbarer Diff-Block für eine Datei — Kopf mit Pfad und Zähler,
/// darunter die Zeilen. Große Diffs starten eingeklappt.
fn diff_file_html(file: &DiffFile, file_href: &BTreeMap<String, String>) -> String {
    let name = match file_href.get(&file.path) {
        Some(href) => format!(
            "<a href=\"{href}\">{path}</a>",
            href = escape(href),
            path = escape(&file.path),
        ),
        None => format!("<code>{}</code>", escape(&file.path)),
    };
    // Große Dateien nicht sofort ausbreiten — wie in GitLab.
    let open = if file.lines.len() > 120 { "" } else { " open" };
    let inner = if file.binary {
        "<p class=\"empty\">Binärdatei — nicht als Text darstellbar.</p>\n".to_string()
    } else {
        format!(
            "<div class=\"diff-body\"><table class=\"diff-table\">\n{rows}</table></div>\n",
            rows = diff_rows_html(&file.lines),
        )
    };
    format!(
        "<details class=\"diff\"{open}>\n\
           <summary><span class=\"diff-path\">{name}</span>\
             <span class=\"stat\"><span class=\"add\">+{added}</span> \
             <span class=\"del\">-{removed}</span></span></summary>\n\
           {inner}\
         </details>\n",
        added = file.added,
        removed = file.removed,
    )
}

/// Die Zeilen eines Datei-Diffs als Tabellenzeilen: zwei Nummern-Spalten (alt,
/// neu) und der Code, eingefärbt nach [`DiffKind`].
fn diff_rows_html(lines: &[DiffLine]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.kind == DiffKind::Hunk {
            out.push_str(&format!(
                "<tr class=\"hunk\"><td class=\"ln\"></td><td class=\"ln\"></td>\
                 <td class=\"code\">{}</td></tr>\n",
                escape(&line.text),
            ));
            continue;
        }
        let (cls, sign) = match line.kind {
            DiffKind::Added => ("add", "+"),
            DiffKind::Removed => ("del", "-"),
            _ => ("ctx", " "),
        };
        out.push_str(&format!(
            "<tr class=\"{cls}\"><td class=\"ln\">{old}</td><td class=\"ln\">{new}</td>\
             <td class=\"code\"><span class=\"sign\">{sign}</span>{text}</td></tr>\n",
            old = num(line.old),
            new = num(line.new),
            text = escape(&line.text),
        ));
    }
    out
}

/// Eine Zeilennummer als Text, oder leer bei `None`.
fn num(n: Option<u32>) -> String {
    n.map(|n| n.to_string()).unwrap_or_default()
}

/// Die Übersicht: Intent zuerst, Dateien danach.
///
/// Das ist die Reihenfolge aus der Vision — der Reviewer liest, was verlangt
/// wurde, und geht erst dann in den Code.
pub fn index_page(
    index: &Index,
    files: &[FileLink],
    session_page: &BTreeMap<SessionId, String>,
) -> String {
    let body = if index.is_empty() && files.is_empty() {
        empty_state()
    } else {
        let sessions: Vec<Session> = index.sessions().map(|(_, s)| s.clone()).collect();
        let metrics = minds_metrics::Metrics::from_sessions(&sessions);
        format!(
            "<header class=\"top\">\n\
               <h1>Minds</h1>\n\
               <p class=\"meta\">{sessions} Session(s) · {commits} Commit(s) mit Kontext · {files} Datei(en){damaged}</p>\n\
             </header>\n\
             <main class=\"overview\">\n\
               {tiles}\
               {activity}\
               <h2>Absichten</h2>\n\
               <input class=\"search\" type=\"search\" placeholder=\"Absichten durchsuchen …\" aria-label=\"Absichten durchsuchen\">\n\
               {session_list}\n\
               <h2>Dateien</h2>\n\
               {file_list}\n\
             </main>\n",
            sessions = index.len(),
            commits = index.attributed_commits(),
            files = files.len(),
            damaged = damaged_note(index.unreadable()),
            tiles = kpi_tiles(&metrics),
            activity = activity_chart(&sessions),
            session_list = session_list(index, session_page),
            file_list = file_list(files),
        )
    };
    page("Minds — Kontext", &body)
}

/// Der Empty-State: kein Kontext erfasst. Ehrlich und mit dem nächsten Schritt.
fn empty_state() -> String {
    "<header class=\"top\"><h1>Minds</h1></header>\n\
     <main class=\"overview\">\n\
       <p class=\"empty\">Für dieses Repository ist noch kein Kontext erfasst.</p>\n\
       <p class=\"hint\">Richte die Hooks mit <code>minds enable</code> ein; \
        beim nächsten Commit legt <code>minds checkpoint</code> die erste Session an.</p>\n\
     </main>\n"
        .to_string()
}

/// Der Hinweis auf nicht auflösbare Sessions — nur, wenn es welche gibt.
fn damaged_note(unreadable: usize) -> String {
    if unreadable == 0 {
        String::new()
    } else {
        format!(" · <span class=\"warn\">{unreadable} nicht lesbar</span>")
    }
}

/// Die Sessions, Intent zuerst — neueste oben. (Absichtslose sind schon im
/// [`Index`] ausgesiebt.)
///
/// Jede Karte verlinkt auf die **Session-Seite**, die Absicht und alle
/// geänderten Dateien samt Änderungen zeigt. Sessions ohne eigene Seite
/// (sollte nicht vorkommen) bleiben eine schlichte Karte.
fn session_list(index: &Index, session_page: &BTreeMap<SessionId, String>) -> String {
    let mut items: Vec<(&SessionId, &Session)> = index.sessions().collect();
    if items.is_empty() {
        return "<p class=\"empty\">Keine Session mit erfasster Absicht.</p>\n".to_string();
    }

    // Neueste zuerst: nach Endzeitpunkt absteigend. Minds-Zeitstempel sind
    // einheitlich RFC 3339 in UTC (…Z), deshalb ordnet der String-Vergleich
    // chronologisch.
    items.sort_by(|a, b| session_time(b.1).cmp(session_time(a.1)));

    // Nach Tag gruppiert (U.6), wie in entires Overview: je Tag eine Überschrift,
    // darunter die Karten. Jede Karte trägt `data-search` für den Client-Filter.
    let mut out = String::new();
    let mut current_day: Option<String> = None;
    for (id, session) in items {
        let day = day_of(session_time(session));
        if current_day.as_deref() != Some(day.as_str()) {
            if current_day.is_some() {
                out.push_str("</ul>\n");
            }
            out.push_str(&format!(
                "<h3 class=\"day\">{}</h3>\n<ul class=\"cards\">\n",
                escape(&day)
            ));
            current_day = Some(day);
        }

        let summary = Summary::of(*id, session);
        let guess = if index.is_observed(*id) {
            ""
        } else {
            " · <span class=\"guess\">vermutet</span>"
        };
        let inner = format!(
            "<p class=\"headline\">{headline} <span class=\"agent-badge\">{agent}</span></p>\n\
             <p class=\"sub\">{actor} · {files} Datei(en) · {input} ein / {output} aus Token{guess}</p>\n\
             <p class=\"sub\"><code class=\"id\">{id}</code></p>\n",
            headline = escape(&summary.headline),
            agent = escape(&session.agent.name),
            actor = escape(&summary.actor),
            files = summary.files,
            input = summary.input_tokens,
            output = summary.output_tokens,
            id = escape(&summary.id.to_string()),
        );
        let key = escape(&format!("{} {}", summary.headline, summary.id).to_lowercase());

        match session_page.get(id) {
            Some(href) => out.push_str(&format!(
                "<li class=\"card\" data-search=\"{key}\"><a class=\"card-link\" href=\"{href}\">{inner}</a></li>\n",
                href = escape(href),
            )),
            None => out.push_str(&format!("<li class=\"card\" data-search=\"{key}\">{inner}</li>\n")),
        }
    }
    if current_day.is_some() {
        out.push_str("</ul>\n");
    }
    out
}

/// Der Datumsteil (`YYYY-MM-DD`) eines Zeitstempels für die Tagesgruppierung;
/// leer wird zu „ohne Datum".
fn day_of(time: &str) -> String {
    match time.get(..10) {
        Some(day) if !day.is_empty() => day.to_string(),
        _ => "ohne Datum".to_string(),
    }
}

/// Die vier KPI-Kacheln (U.4) — dieselben wie in entires Overview, aus
/// `minds-metrics`.
fn kpi_tiles(m: &minds_metrics::Metrics) -> String {
    let throughput = thousands(m.throughput.round() as u64);
    let iteration = format!("{:.1}", m.iteration);
    let continuity = human_duration(m.continuity_seconds);
    format!(
        "<div class=\"tiles\">\
           <div class=\"tile\"><span class=\"tval\">{throughput}</span><span class=\"tlbl\">Ø Token / Session</span></div>\
           <div class=\"tile\"><span class=\"tval\">{iteration}</span><span class=\"tlbl\">Ø Tool-Calls / Session</span></div>\
           <div class=\"tile\"><span class=\"tval\">{continuity}</span><span class=\"tlbl\">längste Session</span></div>\
           <div class=\"tile\"><span class=\"tval\">{streak}</span><span class=\"tlbl\">Tage Streak</span></div>\
         </div>\n",
        streak = m.streak_current_days,
    )
}

/// Der Aktivitäts-Chart (U.5): Sessions je Tag der letzten 30 Tage als
/// Inline-SVG-Balken. Leer, wenn keine Session einen Zeitstempel trägt.
fn activity_chart(sessions: &[Session]) -> String {
    let mut per_day: BTreeMap<i64, u32> = BTreeMap::new();
    for session in sessions {
        if let Some(day) = session
            .lineage
            .as_ref()
            .and_then(|l| l.started_at.as_deref())
            .and_then(minds_metrics::day_number)
        {
            *per_day.entry(day).or_default() += 1;
        }
    }
    let Some(&max_day) = per_day.keys().max() else {
        return String::new();
    };
    let start = max_day - 29;
    let max_count = per_day.values().copied().max().unwrap_or(1).max(1);

    let mut bars = String::new();
    for (i, day) in (start..=max_day).enumerate() {
        let count = per_day.get(&day).copied().unwrap_or(0);
        if count == 0 {
            continue;
        }
        let h = (count as f64 / max_count as f64 * 46.0).max(2.0);
        let x = i as f64 * 10.0;
        let y = 50.0 - h;
        bars.push_str(&format!(
            "<rect x=\"{x:.0}\" y=\"{y:.1}\" width=\"8\" height=\"{h:.1}\" rx=\"1\"><title>{day_count}</title></rect>",
            day_count = count,
        ));
    }
    format!(
        "<div class=\"activity\">\
           <svg viewBox=\"0 0 300 52\" preserveAspectRatio=\"none\" class=\"actsvg\" aria-label=\"Aktivität der letzten 30 Tage\">{bars}</svg>\
           <p class=\"sub\">Aktivität der letzten 30 Tage</p>\
         </div>\n"
    )
}

/// Sekunden als grobe, lesbare Dauer: `21h 5m`, `12m`, `45s`.
fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Große Zahlen mit Tausenderpunkt (deutsch): `20.940.890`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// Der Sortierschlüssel einer Session: ihr Endzeitpunkt, ersatzweise ihr
/// Beginn, sonst leer (die dann nach unten sortieren).
fn session_time(session: &Session) -> &str {
    session
        .lineage
        .as_ref()
        .and_then(|l| l.ended_at.as_deref().or(l.started_at.as_deref()))
        .unwrap_or("")
}

/// Die Dateien mit Kontext, jede ein Link auf ihre Seite.
fn file_list(files: &[FileLink]) -> String {
    if files.is_empty() {
        return "<p class=\"empty\">Keine Datei mit erfasstem Kontext.</p>\n".to_string();
    }

    let mut out = String::from("<ul class=\"files\">\n");
    for file in files {
        out.push_str(&format!(
            "<li><a href=\"{href}\"><code>{path}</code></a> \
             <span class=\"sub\">{attributed}/{total} Zeilen</span></li>\n",
            href = escape(&file.href),
            path = escape(&file.path),
            attributed = file.attributed,
            total = file.total,
        ));
    }
    out.push_str("</ul>\n");
    out
}

/// Ein Dateiname für die Seite einer Datei.
///
/// Aus `src/retry.rs` wird `src-retry.rs.html`. Erlaubt bleiben nur
/// `[A-Za-z0-9._-]`; alles andere — Schrägstriche, Leerzeichen, Umlaute — wird
/// zu `-`. Damit ist der Name auf jedem Dateisystem und in jeder URL
/// unverfänglich.
///
/// Die Abbildung ist **nicht** injektiv (`a/b` und `a-b` ergeben dasselbe); die
/// Eindeutigkeit stellt der Aufrufer her, indem er Kollisionen durchnummeriert
/// — siehe `render`.
pub fn slug(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 5);
    for ch in path.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("datei");
    }
    out.push_str(".html");
    out
}

/// `text`, oder `fallback`, wenn `text` leer ist.
fn non_empty<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.trim().is_empty() {
        fallback
    } else {
        text
    }
}

/// Maskiert die fünf Zeichen, die in HTML-Text und -Attributen gefährlich sind.
///
/// Der Reader schreibt fremden Text in HTML: Prompts, Dateipfade, Modellnamen.
/// Ohne Maskierung wäre ein Prompt mit `<script>` eine Lücke — und Prompts sind
/// per Definition beliebiger Text. Deshalb geht **jeder** eingesetzte Wert hier
/// durch, ausnahmslos.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Das Stylesheet. Bewusst knapp und ohne Abhängigkeit; hell und dunkel über
/// `prefers-color-scheme`.
const STYLE: &str = "\
:root{--bg:#fff;--fg:#1a1a1a;--dim:#666;--rule:#e3e3e3;--mark:#f0f6ff;--accent:#2f6feb;--warn:#b3261e;--hl-kw:#8250df;--hl-str:#0a7d33;--hl-num:#b35900;--hl-com:#6a737d}\
@media(prefers-color-scheme:dark){:root{--bg:#14151a;--fg:#e6e6e6;--dim:#9aa0a6;--rule:#2a2c33;--mark:#1b2740;--accent:#7aa7ff;--warn:#f2b8b5;--hl-kw:#c678dd;--hl-str:#98c379;--hl-num:#d19a66;--hl-com:#8b949e}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}\
a{color:var(--accent)}\
.top{padding:1rem 1.25rem;border-bottom:1px solid var(--rule)}\
.top h1{margin:.25rem 0;font-size:1.1rem;font-family:ui-monospace,monospace;word-break:break-all}\
.meta{margin:0;color:var(--dim);font-size:.85rem}\
.split{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,26rem);gap:0;align-items:start}\
@media(max-width:820px){.split{grid-template-columns:minmax(0,1fr)}}\
.code{overflow-x:auto;padding:.5rem 0;font:13px/1.6 ui-monospace,monospace}\
.line{display:flex;gap:.75rem;padding:0 1rem;white-space:pre}\
.line .num{color:var(--dim);text-align:right;min-width:3.5ch;user-select:none}\
.line code{white-space:pre}\
.line.has-context{background:var(--mark);border-left:3px solid var(--accent);padding-left:calc(1rem - 3px);cursor:pointer}\
.line.has-context:hover,.line.has-context:focus{outline:none;filter:brightness(1.06)}\
.line.selected{box-shadow:inset 0 0 0 2px var(--accent)}\
.panel{position:sticky;top:0;max-height:100vh;overflow-y:auto;padding:1rem 1.25rem;border-left:1px solid var(--rule)}\
@media(max-width:820px){.panel{position:static;max-height:none;border-left:0;border-top:1px solid var(--rule)}}\
.panel h2{font-size:.8rem;text-transform:uppercase;letter-spacing:.06em;color:var(--dim);margin:1rem 0 .35rem}\
.panel h3{font-size:.75rem;text-transform:uppercase;letter-spacing:.06em;color:var(--dim);margin:1rem 0 .25rem}\
.intent{margin:0;font-size:1rem}\
.hint{color:var(--dim);font-size:.85rem}\
dl{margin:0;display:grid;grid-template-columns:auto 1fr;gap:.15rem .75rem;font-size:.9rem}\
dt{color:var(--dim)}\
dd{margin:0;word-break:break-all}\
ul{margin:.25rem 0;padding-left:1.1rem;font-size:.9rem}\
code.id{font-size:.75rem;word-break:break-all}\
.warn{color:var(--warn)}\
.back{font-size:.85rem;text-decoration:none}\
.overview{padding:1rem 1.25rem;max-width:60rem}\
.overview h2{font-size:.8rem;text-transform:uppercase;letter-spacing:.06em;color:var(--dim);margin:1.5rem 0 .5rem}\
.cards{list-style:none;margin:0;padding:0;display:grid;gap:.5rem}\
.card{border:1px solid var(--rule);border-left:3px solid var(--accent);border-radius:.35rem;padding:0}\
.card-link{display:block;padding:.6rem .8rem;color:inherit;text-decoration:none}\
.card:has(.card-link){cursor:pointer}\
.card:has(.card-link):hover{background:var(--mark)}\
.card>.headline{padding:.6rem .8rem .2rem}\
.card>.sub{padding:0 .8rem .6rem}\
.headline{margin:0 0 .2rem;font-size:1rem}\
.sub{margin:0;color:var(--dim);font-size:.8rem}\
.files{list-style:none;margin:0;padding:0;display:grid;gap:.25rem}\
.files a{text-decoration:none}\
.files a:hover{text-decoration:underline}\
.empty{color:var(--dim)}\
.guess{color:var(--warn)}\
.session .guess{font-size:.8rem;margin:.25rem 0 0}\
.session-view{padding:1rem 1.25rem;max-width:70rem}\
.session-view .session{border:0;padding:0}\
.session-view .session h2,.session-view .session h3,.changes-h{font-size:.75rem;text-transform:uppercase;letter-spacing:.06em;color:var(--dim);margin:1.25rem 0 .35rem}\
.session-view .session .intent{margin:0 0 .5rem;font-size:1.05rem}\
.changes-h{margin-top:1.75rem}\
details.diff{border:1px solid var(--rule);border-radius:.35rem;margin:.5rem 0;overflow:hidden}\
details.diff>summary{cursor:pointer;padding:.5rem .7rem;background:var(--mark);display:flex;gap:.6rem;align-items:center;font-size:.85rem;list-style:none}\
details.diff>summary::-webkit-details-marker{display:none}\
details.diff>summary::before{content:'▸';color:var(--dim)}\
details.diff[open]>summary::before{content:'▾'}\
.diff-path{min-width:0;overflow-wrap:anywhere}\
.diff-path code,.diff-path a{font-family:ui-monospace,monospace;text-decoration:none;color:var(--accent)}\
.stat{margin-left:auto;font-family:ui-monospace,monospace;white-space:nowrap}\
.stat .add{color:#2da44e}.stat .del{color:var(--warn)}\
.diff-body{overflow-x:auto}\
.diff-table{border-collapse:collapse;width:100%;font:12px/1.55 ui-monospace,monospace}\
.diff-table td{padding:0 .5rem;white-space:pre;vertical-align:top}\
.diff-table td.ln{text-align:right;color:var(--dim);user-select:none;min-width:3ch;border-right:1px solid var(--rule)}\
.diff-table td.code{width:100%}\
.diff-table .sign{display:inline-block;width:1ch;user-select:none;color:var(--dim)}\
.diff-table tr.add td.code{background:rgba(45,164,78,.12)}\
.diff-table tr.del td.code{background:rgba(179,38,30,.13)}\
.diff-table tr.add .sign{color:#2da44e}\
.diff-table tr.del .sign{color:var(--warn)}\
.diff-table tr.hunk td{background:var(--rule);color:var(--dim)}\
.attrbar{height:6px;background:var(--rule);border-radius:3px;overflow:hidden;margin:.45rem 0}\
.attrbar-fill{height:100%;background:var(--accent)}\
.timeline{display:grid;gap:.5rem;margin:.25rem 0}\
.turn{border-left:2px solid var(--rule);padding:.1rem 0 .1rem .6rem}\
.turn.user{border-color:var(--accent)}\
.turn.assistant{border-color:var(--dim)}\
.turn .role{font-size:.68rem;text-transform:uppercase;letter-spacing:.06em;color:var(--dim)}\
.turn-text{white-space:pre-wrap;overflow-wrap:anywhere;font-size:.9rem;margin:.15rem 0}\
details.tools{margin:.2rem 0}\
details.tools>summary{cursor:pointer;font-size:.78rem;color:var(--dim)}\
.toollist{list-style:none;margin:.2rem 0;padding:0;display:grid;gap:.2rem;font-size:.82rem}\
.tool-name{font-family:ui-monospace,monospace}\
.tool-detail{color:var(--dim);overflow-wrap:anywhere}\
.effect{font-size:.66rem;text-transform:uppercase;letter-spacing:.04em;padding:.02rem .3rem;border-radius:.25rem;background:var(--rule);color:var(--dim)}\
.effect.write{background:rgba(45,164,78,.18);color:#2da44e}\
.effect.read{background:var(--mark);color:var(--accent)}\
.effect.exec{background:rgba(179,38,30,.13);color:var(--warn)}\
.effect.delete{background:rgba(179,38,30,.22);color:var(--warn)}\
.tiles{display:grid;grid-template-columns:repeat(4,1fr);gap:.6rem;margin:1rem 0}\
@media(max-width:640px){.tiles{grid-template-columns:repeat(2,1fr)}}\
.tile{border:1px solid var(--rule);border-radius:.4rem;padding:.6rem .8rem;display:flex;flex-direction:column;gap:.15rem}\
.tval{font:1.4rem/1 ui-monospace,monospace;font-weight:600}\
.tlbl{font-size:.7rem;color:var(--dim);text-transform:uppercase;letter-spacing:.04em}\
.activity{margin:.5rem 0 1rem}\
.actsvg{width:100%;height:52px;display:block}\
.actsvg rect{fill:var(--accent)}\
.day{font-size:.72rem;color:var(--dim);margin:1.1rem 0 .35rem;font-weight:600}\
.search{width:100%;max-width:24rem;padding:.4rem .6rem;margin:.25rem 0 .75rem;border:1px solid var(--rule);border-radius:.35rem;background:var(--bg);color:var(--fg);font:inherit}\
.hl-kw{color:var(--hl-kw)}\
.hl-str{color:var(--hl-str)}\
.hl-num{color:var(--hl-num)}\
.hl-com{color:var(--hl-com);font-style:italic}\
.agent-badge{display:inline-block;font-size:.68rem;padding:.02rem .35rem;border-radius:.25rem;background:var(--mark);color:var(--accent);font-family:ui-monospace,monospace}\
";

/// Das Skript: genau eine Aufgabe — das Panel zur angeklickten Zeile zeigen.
///
/// Ohne JavaScript bleibt die Seite lesbar; dann sind alle Panels sichtbar,
/// weil `hidden` erst hier gesetzt/entfernt wird.
const SCRIPT: &str = "\
(function(){\
var panel=document.getElementById('panel');\
if(panel){\
/* Erst hier verstecken: ohne Skript bleiben alle Panels lesbar. */\
panel.querySelectorAll('.session').forEach(function(s){s.hidden=true});\
var show=function(row){\
var ids=(row.getAttribute('data-sessions')||'').split(' ').filter(Boolean);\
panel.querySelectorAll('.session').forEach(function(s){s.hidden=true});\
ids.forEach(function(id){var el=document.getElementById('s-'+id);if(el)el.hidden=false});\
document.querySelectorAll('.line.selected').forEach(function(l){l.classList.remove('selected')});\
row.classList.add('selected');\
var hint=panel.querySelector('.hint');if(hint)hint.hidden=ids.length>0;\
};\
document.addEventListener('click',function(e){\
var row=e.target.closest('.line.has-context');if(row)show(row);\
});\
document.addEventListener('keydown',function(e){\
if(e.key!=='Enter'&&e.key!==' ')return;\
var row=e.target.closest&&e.target.closest('.line.has-context');\
if(row){e.preventDefault();show(row)}\
});\
/* Von der Übersicht verlinkt (#s-<id>): die erste Zeile dieser Session zeigen. */\
var openHash=function(){\
var h=location.hash;if(h.indexOf('#s-')!==0)return;\
var id=h.slice(3);\
var row=document.querySelector('.line.has-context[data-sessions~=\"'+id+'\"]');\
if(row){show(row);row.scrollIntoView({block:'center'})}\
};\
openHash();\
window.addEventListener('hashchange',openHash);\
}\
/* Übersicht: die Karten nach Absicht/Id filtern (U.6). */\
var search=document.querySelector('.search');\
if(search){search.addEventListener('input',function(){\
var q=search.value.toLowerCase();\
document.querySelectorAll('.cards').forEach(function(ul){\
var any=false;\
ul.querySelectorAll('.card').forEach(function(c){\
var m=(c.getAttribute('data-search')||'').indexOf(q)>=0;\
c.hidden=!m;if(m)any=true;\
});\
var day=ul.previousElementSibling;\
if(day&&day.classList.contains('day'))day.hidden=!any;\
ul.hidden=!any;\
});\
});}\
})();\
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileView;
    use minds_core::{Agent, Intent, Model, Produced, Usage};
    use minds_git::{BlameLine, CommitId};
    use std::collections::BTreeMap;

    fn sid(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    fn cid(hex: char) -> CommitId {
        hex.to_string().repeat(40).parse().unwrap()
    }

    fn session(request: &str) -> Session {
        let mut s = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1.4.2".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "claude-opus-4".into(),
            },
            Intent {
                request: request.into(),
                constraints: vec!["keine neuen Dependencies".into()],
                discarded: vec!["Timeout hochsetzen".into()],
            },
        );
        s.usage = Usage {
            input_tokens: 900,
            output_tokens: 120,
        };
        s.produced = Produced {
            commit_hint: None,
            files: vec!["src/retry.rs".into()],
        };
        s
    }

    fn index_with(request: &str) -> Index {
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), session(request));
        let mut commits = BTreeMap::new();
        commits.insert(cid('1'), vec![sid('a')]);
        Index::from_parts(sessions, commits)
    }

    fn view() -> FileView {
        FileView::join(
            "src/retry.rs",
            "fn retry() {}\nfn plain() {}\n",
            &[BlameLine {
                line: 1,
                commit: cid('1'),
            }],
            &index_with("Der Retry-Test flackert"),
        )
    }

    #[test]
    fn escapes_the_dangerous_five() {
        assert_eq!(escape(r#"<script>&"'"#), "&lt;script&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn a_prompt_cannot_inject_markup() {
        // Prompts sind per Definition beliebiger Text — der wichtigste Test hier.
        let index = index_with("<img src=x onerror=alert(1)>");
        let html = file_page(&view(), &index);
        assert!(!html.contains("<img src=x"), "ungeschütztes Markup im HTML");
        assert!(html.contains("&lt;img src=x"));
    }

    #[test]
    fn an_attributed_line_is_clickable_and_carries_its_session() {
        let html = file_page(&view(), &index_with("egal"));
        assert!(html.contains("class=\"line has-context\""));
        assert!(html.contains(&format!("data-sessions=\"{}\"", sid('a'))));
    }

    #[test]
    fn a_plain_line_is_not_clickable() {
        let html = file_page(&view(), &index_with("egal"));
        // Zeile 2 hat keinen Blame-Eintrag und darf kein data-sessions tragen.
        assert!(html.contains("<div class=\"line\"><span class=\"num\">2</span>"));
    }

    #[test]
    fn the_panel_holds_the_intent_and_the_origin() {
        let html = file_page(&view(), &index_with("Der Retry-Test flackert"));
        assert!(html.contains("Der Retry-Test flackert"));
        assert!(html.contains("claude-code"));
        assert!(html.contains("claude-opus-4"));
        assert!(html.contains("keine neuen Dependencies"));
        assert!(html.contains("Timeout hochsetzen"));
        assert!(html.contains("900 ein / 120 aus"));
    }

    #[test]
    fn the_page_is_self_contained() {
        let html = file_page(&view(), &index_with("egal"));
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("<script>"));
        // Kein Verweis nach draußen.
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn an_orphan_link_is_filtered_out_of_the_reader() {
        // Der Blame zeigt auf einen Commit, dessen Session der Store nicht hat.
        // Der Reader-Index lässt solche Verweise gar nicht erst durch (fsck
        // meldet sie stattdessen systematisch) — die Zeile ist dann schlicht
        // nicht zugeordnet, kein Panel, keine „Waise" auf der Seite.
        let mut commits = BTreeMap::new();
        commits.insert(cid('1'), vec![sid('f')]);
        let index = Index::from_parts(BTreeMap::new(), commits);
        let view = FileView::join(
            "a.rs",
            "eins\n",
            &[BlameLine {
                line: 1,
                commit: cid('1'),
            }],
            &index,
        );
        assert!(!view.is_attributed(), "der verwaiste Verweis ist gefiltert");
        let html = file_page(&view, &index);
        assert!(!html.contains("verwaist"));
    }

    #[test]
    fn a_missing_prompt_says_so() {
        let html = session_panel(sid('a'), &session(""), false);
        assert!(html.contains("(kein Prompt erfasst)"));
    }

    #[test]
    fn an_inferred_panel_is_marked_a_guess() {
        let observed = session_panel(sid('a'), &session("x"), false);
        assert!(!observed.contains("vermutet"));
        let inferred = session_panel(sid('a'), &session("x"), true);
        assert!(inferred.contains("vermutet"));
    }

    #[test]
    fn the_panel_renders_the_turn_timeline_with_tools() {
        use minds_core::{Effect, EffectKind, Role, ToolCall, Turn};
        let mut s = session("Fix retry");
        s.turns.push(Turn {
            role: Role::User,
            text: "Fix it".into(),
            tool_calls: vec![],
            parent: None,
            at: None,
        });
        s.turns.push(Turn {
            role: Role::Assistant,
            text: "Ok".into(),
            tool_calls: vec![
                ToolCall {
                    capture: None,
                    name: "Bash".into(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                    effect: Some(Effect {
                        kind: EffectKind::Exec,
                        path: None,
                        content: None,
                    }),
                },
                ToolCall {
                    capture: None,
                    name: "Edit".into(),
                    arguments: "{}".into(),
                    effect: Some(Effect {
                        kind: EffectKind::Write,
                        path: Some("src/retry.rs".into()),
                        content: None,
                    }),
                },
            ],
            parent: None,
            at: None,
        });
        let html = session_panel(sid('a'), &s, false);
        assert!(html.contains("Verlauf"));
        assert!(html.contains("class=\"turn user\""));
        assert!(html.contains("class=\"turn assistant\""));
        // Exec-Kommando entrauscht sichtbar, Write-Effekt als Badge mit Pfad.
        assert!(html.contains("cargo test"));
        assert!(html.contains("class=\"effect write\""));
        assert!(html.contains("src/retry.rs"));
    }

    #[test]
    fn the_file_page_shows_an_attribution_bar() {
        let html = file_page(&view(), &index_with("egal"));
        assert!(html.contains("class=\"attrbar\""));
        assert!(html.contains("% Agent"));
    }

    // --- Session-Seite ------------------------------------------------------

    fn diff_of(files: Vec<DiffFile>) -> CommitDiff {
        CommitDiff {
            commit: cid('1'),
            files,
        }
    }

    fn changed_file(path: &str, lines: Vec<DiffLine>) -> DiffFile {
        let added = lines.iter().filter(|l| l.kind == DiffKind::Added).count();
        let removed = lines.iter().filter(|l| l.kind == DiffKind::Removed).count();
        DiffFile {
            path: path.into(),
            added,
            removed,
            binary: false,
            lines,
        }
    }

    fn line(kind: DiffKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine {
            kind,
            old,
            new,
            text: text.into(),
        }
    }

    #[test]
    fn a_session_page_shows_every_changed_file_collapsible() {
        // Zwei Dateien in einem Commit — genau das Feedback: nicht nur die erste.
        let diffs = vec![diff_of(vec![
            changed_file(
                ".claude/settings.json",
                vec![line(DiffKind::Added, None, Some(1), "{}")],
            ),
            changed_file(
                "test.txt",
                vec![
                    line(DiffKind::Hunk, None, None, "@@ -0,0 +1,2 @@"),
                    line(DiffKind::Added, None, Some(1), "hallo"),
                    line(DiffKind::Added, None, Some(2), "welt"),
                ],
            ),
        ])];
        let empty = BTreeMap::new();
        let html = session_page(sid('a'), &session("meine Absicht"), &diffs, false, &empty);

        // Beide Dateien tauchen auf, jede in einem aufklappbaren Block.
        assert!(html.contains(".claude/settings.json"));
        assert!(html.contains("test.txt"));
        assert_eq!(html.matches("<details class=\"diff\"").count(), 2);
        // Die eigentlichen Änderungen stehen drin.
        assert!(html.contains("hallo"));
        assert!(html.contains("welt"));
        // Und die Absicht oben.
        assert!(html.contains("meine Absicht"));
    }

    #[test]
    fn a_changed_file_links_to_its_line_view_when_present() {
        let diffs = vec![diff_of(vec![changed_file(
            "src/x.rs",
            vec![line(DiffKind::Added, None, Some(1), "neu")],
        )])];
        let mut file_href = BTreeMap::new();
        file_href.insert("src/x.rs".to_string(), "src-x.rs.html".to_string());
        let html = session_page(sid('a'), &session("egal"), &diffs, false, &file_href);
        assert!(html.contains("href=\"src-x.rs.html\""));
    }

    #[test]
    fn a_session_without_a_commit_says_so() {
        let empty = BTreeMap::new();
        let html = session_page(sid('a'), &session("nur Absicht"), &[], false, &empty);
        assert!(html.contains("nur Absicht"));
        assert!(html.contains("Keine dem Commit zugeordnete Änderung"));
    }

    #[test]
    fn a_diff_line_cannot_inject_markup() {
        let diffs = vec![diff_of(vec![changed_file(
            "x",
            vec![line(
                DiffKind::Added,
                None,
                Some(1),
                "<script>alert(1)</script>",
            )],
        )])];
        let empty = BTreeMap::new();
        let html = session_page(sid('a'), &session("egal"), &diffs, false, &empty);
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;script&gt;"));
    }

    // --- Übersicht ----------------------------------------------------------

    fn links() -> Vec<FileLink> {
        vec![FileLink {
            path: "src/retry.rs".into(),
            href: "src-retry.rs.html".into(),
            attributed: 1,
            total: 2,
        }]
    }

    fn pages() -> BTreeMap<SessionId, String> {
        BTreeMap::new()
    }

    #[test]
    fn the_overview_puts_intent_first() {
        let html = index_page(&index_with("Der Retry-Test flackert"), &links(), &pages());
        let intent = html.find("Der Retry-Test flackert").expect("Absicht fehlt");
        let files = html.find("src/retry.rs").expect("Datei fehlt");
        assert!(intent < files, "Intent muss vor den Dateien stehen");
    }

    #[test]
    fn the_overview_links_to_each_file() {
        let html = index_page(&index_with("egal"), &links(), &pages());
        assert!(html.contains("href=\"src-retry.rs.html\""));
        assert!(html.contains("1/2 Zeilen"));
    }

    #[test]
    fn a_session_card_links_to_its_session_page() {
        let mut mapping = BTreeMap::new();
        mapping.insert(sid('a'), "session-aaaaaaaaaaaa.html".to_string());
        let html = index_page(&index_with("egal"), &links(), &mapping);
        assert!(
            html.contains("class=\"card-link\" href=\"session-aaaaaaaaaaaa.html\""),
            "die Karte muss auf die Session-Seite verlinken:\n{html}"
        );
    }

    #[test]
    fn the_empty_state_names_the_next_step() {
        let html = index_page(&Index::default(), &[], &pages());
        assert!(html.contains("noch kein Kontext erfasst"));
        assert!(html.contains("minds enable"));
        assert!(html.contains("minds checkpoint"));
    }

    #[test]
    fn the_overview_drops_prompt_less_and_sorts_newest_first() {
        use minds_core::Lineage;
        fn at(request: &str, ended: &str) -> Session {
            let mut s = session(request);
            s.lineage = Some(Lineage {
                local_id: "x".into(),
                started_at: None,
                ended_at: Some(ended.into()),
                cwd: None,
            });
            s
        }
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), at("die alte Arbeit", "2026-01-01T00:00:00.000Z"));
        sessions.insert(sid('b'), at("die neue Arbeit", "2026-09-09T00:00:00.000Z"));
        sessions.insert(sid('c'), at("", "2026-05-05T00:00:00.000Z")); // ohne Absicht
        let index = Index::from_parts(sessions, BTreeMap::new());

        let html = index_page(&index, &[], &pages());
        // Die absichtslose Session taucht nicht auf.
        assert!(!html.contains(&sid('c').to_string()));
        // Neu vor alt.
        let neu = html.find("die neue Arbeit").unwrap();
        let alt = html.find("die alte Arbeit").unwrap();
        assert!(neu < alt, "neueste zuerst");
    }

    #[test]
    fn an_overview_prompt_cannot_inject_markup() {
        let html = index_page(&index_with("<script>alert(1)</script>"), &links(), &pages());
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_overview_has_kpi_tiles_and_a_search_box() {
        let html = index_page(&index_with("egal"), &links(), &pages());
        assert!(html.contains("class=\"tiles\""));
        assert!(html.contains("Ø Token / Session"));
        assert!(html.contains("class=\"search\""));
    }

    #[test]
    fn the_overview_groups_sessions_by_day_and_charts_activity() {
        use minds_core::Lineage;
        fn at(request: &str, started: &str) -> Session {
            let mut s = session(request);
            s.lineage = Some(Lineage {
                local_id: "x".into(),
                started_at: Some(started.into()),
                ended_at: Some(started.into()),
                cwd: None,
            });
            s
        }
        let mut sessions = BTreeMap::new();
        sessions.insert(sid('a'), at("heute A", "2026-07-25T09:00:00Z"));
        sessions.insert(sid('b'), at("gestern B", "2026-07-24T09:00:00Z"));
        let index = Index::from_parts(sessions, BTreeMap::new());

        let html = index_page(&index, &[], &pages());
        assert!(html.contains("class=\"day\">2026-07-25"));
        assert!(html.contains("class=\"day\">2026-07-24"));
        assert!(html.contains("class=\"actsvg\""), "Aktivitäts-Chart fehlt");
        assert!(
            html.contains("data-search="),
            "Karten brauchen den Suchschlüssel"
        );
    }

    #[test]
    fn duration_and_thousands_format_readably() {
        assert_eq!(human_duration(75549), "20h 59m");
        assert_eq!(human_duration(600), "10m");
        assert_eq!(human_duration(45), "45s");
        assert_eq!(thousands(20940890), "20.940.890");
        assert_eq!(thousands(5), "5");
    }

    #[test]
    fn a_session_card_carries_an_agent_badge() {
        let html = index_page(&index_with("egal"), &links(), &pages());
        assert!(html.contains("class=\"agent-badge\">claude-code"));
    }

    // --- U.7: Highlighting --------------------------------------------------

    #[test]
    fn highlighting_marks_keywords_strings_and_comments() {
        let lang = lang_of("x.rs");
        let out = highlight("let x = \"hi\"; // note", lang.as_ref());
        assert!(out.contains("<span class=\"hl-kw\">let</span>"));
        assert!(out.contains("<span class=\"hl-str\">&quot;hi&quot;</span>"));
        assert!(out.contains("<span class=\"hl-com\">// note</span>"));
    }

    #[test]
    fn highlighting_still_escapes_dangerous_markup() {
        // Der Kern-Vertrag: auch mit Highlighting kann Code kein Markup injizieren.
        let lang = lang_of("x.js");
        let out = highlight("const a = \"<script>alert(1)</script>\"", lang.as_ref());
        assert!(!out.contains("<script>alert(1)"));
        assert!(out.contains("&lt;script&gt;"));
        assert!(out.contains("<span class=\"hl-kw\">const</span>"));
    }

    #[test]
    fn an_unknown_language_is_just_escaped() {
        assert!(lang_of("notes.txt").is_none());
        assert_eq!(highlight("<b>x</b>", None), "&lt;b&gt;x&lt;/b&gt;");
    }

    #[test]
    fn slug_makes_a_safe_filename() {
        assert_eq!(slug("src/retry.rs"), "src-retry.rs.html");
        assert_eq!(slug("a b/c.rs"), "a-b-c.rs.html");
        assert_eq!(
            slug("crates/minds-core/src/lib.rs"),
            "crates-minds-core-src-lib.rs.html"
        );
        // Nichts Gefährliches überlebt: kein Schrägstrich, kein Ausbruch.
        assert_eq!(slug("../../etc/passwd"), "..-..-etc-passwd.html");
        assert!(!slug("../x").contains('/'));
    }

    #[test]
    fn slug_never_produces_an_empty_name() {
        assert_eq!(slug(""), "datei.html");
        assert_eq!(slug("///"), "---.html");
    }

    #[test]
    fn the_page_is_readable_without_javascript() {
        // Die Panels werden sichtbar ausgeliefert; erst das Skript versteckt
        // sie. Stünde `hidden` schon im Markup, wäre die Seite ohne JS leer.
        let html = file_page(&view(), &index_with("Der Retry-Test flackert"));
        assert!(
            !html.contains("class=\"session\" id=\"s-") || !html.contains("\" hidden>"),
            "Panels dürfen nicht als hidden ausgeliefert werden"
        );
        assert!(
            html.contains("s.hidden=true"),
            "das Skript muss sie verstecken"
        );
    }

    #[test]
    fn an_attributed_line_announces_itself() {
        let html = file_page(&view(), &index_with("egal"));
        assert!(html.contains("role=\"button\""));
        assert!(html.contains("Session hinter dieser Zeile anzeigen"));
    }
}
