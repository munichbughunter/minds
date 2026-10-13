//! Architekturregel `no-import`: Keine Datei unter `paths` nennt einen
//! verbotenen Rust-Pfad.
//!
//! Kein Parser (es gibt kein `syn` im Workspace), aber mehr als ein
//! Zeilenabgleich: Kommentare und String-Literale werden ausgeblendet,
//! Leerraum um `::` entfernt, und jede `use`-Deklaration wird als Baum
//! gelesen — auch über Zeilen, verschachtelt (`use a::{b::{c, d}, e};`),
//! mit führendem `::` und `as`. Ein Treffer ist ein importierter Pfad, der
//! mit dem verbotenen beginnt, oder der verbotene Pfad irgendwo im Code als
//! ganze Bezeichnerfolge (`minds_reader::x()`). Eine Crate, die ein
//! `Cargo.toml` umbenennt (`foo = { package = "minds-reader" }`), zählt
//! unter beiden Namen.
//!
//! Was der Scan nicht sicher beurteilen kann, ist UNKNOWN statt PASS: eine
//! `use`-Deklaration, die er nicht lesen kann, `#[path = …]` (ein Modul aus
//! einer anderen Datei), `include!`, ein Glob-Import, der den verbotenen
//! Pfad mitbringen könnte (`use crate::*;`). Grenzen, die bleiben: relative
//! Pfade (`super::x`, `self::x`) werden nicht auf die Crate-Wurzel
//! aufgelöst, und per Makro erzeugter Code wird nicht gesehen.

use std::collections::{BTreeSet, HashSet};

use super::{Judged, MAX_SOURCE, Outcome, Snapshot, manifest};
use crate::text::sanitize_path;

/// Höchstzahl der Pfade, die eine `use`-Deklaration ergeben darf.
const MAX_USE_PATHS: usize = 4_096;

/// So viele Runden folgt der Scan Umbenennungen (`use a as b; use b as c;`).
const MAX_ALIAS_ROUNDS: usize = 8;

/// Höchste Verschachtelung eines `use`-Baums.
const MAX_USE_DEPTH: usize = 32;

pub(super) fn no_import(snapshot: &Snapshot<'_>, paths: &[String], import: &[String]) -> Judged {
    let patterns = match patterns(snapshot, import) {
        Ok(patterns) => patterns,
        Err(reason) => return (Outcome::Unknown, reason),
    };
    let files = match snapshot.matching(paths) {
        Ok(files) => files,
        Err(reason) => return (Outcome::Unknown, reason),
    };
    if files.is_empty() {
        return (Outcome::Unknown, "no file matches these paths".into());
    }
    let mut hits: Vec<(String, usize, String)> = Vec::new();
    for (path, blob) in &files {
        let bytes = match snapshot.read(path, *blob, MAX_SOURCE) {
            Ok(bytes) => bytes,
            Err(reason) => return (Outcome::Unknown, reason),
        };
        let text = String::from_utf8_lossy(&bytes);
        match scan(&text, &patterns) {
            Scan::Clean => {}
            Scan::Hit { line, pattern } => hits.push((sanitize_path(path), line, pattern)),
            Scan::Unreadable(reason) => {
                return (
                    Outcome::Unknown,
                    format!("{}: {reason}", sanitize_path(path)),
                );
            }
        }
    }
    match hits.as_slice() {
        [] => (
            Outcome::Pass,
            format!("not used in {} file(s)", files.len()),
        ),
        [(path, line, what), rest @ ..] => {
            let more = if rest.is_empty() {
                String::new()
            } else {
                format!(" (+{} more)", rest.len())
            };
            (Outcome::Drift, format!("{path}:{line} uses {what}{more}"))
        }
    }
}

/// Die verbotenen Pfade samt den Namen, unter denen ein `Cargo.toml` eine
/// verbotene Crate umbenennt.
fn patterns(snapshot: &Snapshot<'_>, import: &[String]) -> Result<Vec<String>, String> {
    let mut out: BTreeSet<String> = import.iter().cloned().collect();
    let crate_level = import
        .iter()
        .any(|path| !matches!(first(path), "crate" | "self" | "super"));
    if !crate_level {
        return Ok(out.into_iter().collect());
    }
    let graph = manifest::graph(snapshot)?;
    if let Some(what) = graph.unreadable.first() {
        return Err(format!(
            "{what} is not readable — renamed crates cannot be ruled out"
        ));
    }
    for path in import {
        let head = first(path);
        let rest = &path[head.len()..];
        for (package, aliases) in &graph.aliases {
            if package.replace('-', "_") == head {
                for alias in aliases {
                    out.insert(format!("{alias}{rest}"));
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

fn first(path: &str) -> &str {
    path.split("::").next().unwrap_or(path)
}

/// Was eine Datei ergibt.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Scan {
    Clean,
    Hit { line: usize, pattern: String },
    Unreadable(&'static str),
}

/// Durchsucht eine Quelldatei.
pub(crate) fn scan(source: &str, patterns: &[String]) -> Scan {
    let code = blank(source);
    // Fremder Code, den diese Datei hereinholt: nicht beurteilbar.
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    for marker in ["#[path=", "include!("] {
        if bounded_find(&compact, marker) {
            return Scan::Unreadable("pulls in code from another file (#[path] or include!)");
        }
    }
    let normal = normalize(&code);
    // `use`-Deklarationen: erst alle lesen.
    let mut uses: Vec<(usize, Vec<Imported>)> = Vec::new();
    let mut from = 0;
    while let Some(offset) = find_word(&normal[from..], "use") {
        let start = from + offset;
        let after = start + 3;
        let Some(end) = normal[after..].find(';') else {
            return Scan::Unreadable("an unterminated use declaration");
        };
        let tree = normal[after..after + end].trim();
        let mut imported = Vec::new();
        if flatten("", tree, 0, &mut imported).is_none() {
            return Scan::Unreadable("a use declaration the scanner cannot read");
        }
        uses.push((start, imported));
        from = after + end;
    }
    // Ein Import eines **Präfixes** bringt den verbotenen Pfad unter seinem
    // lokalen Namen in den Bereich: `use crate::evidence;` macht
    // `evidence::store` zu `crate::evidence::store`, `use x::{self as r};`
    // macht `r::…` zu `x::…`. Auch das ist ein verbotener Pfad — bis zu
    // einer festen Tiefe (Ketten von Umbenennungen).
    let total: usize = uses.iter().map(|(_, imported)| imported.len()).sum();
    if total > MAX_USE_PATHS {
        return Scan::Unreadable("more imports than the scanner follows");
    }
    let mut all: Vec<(String, String)> = patterns
        .iter()
        .map(|pattern| (pattern.clone(), pattern.clone()))
        .collect();
    let mut known: HashSet<String> = patterns.iter().cloned().collect();
    let mut settled = false;
    for _ in 0..MAX_ALIAS_ROUNDS {
        let mut derived: Vec<(String, String)> = Vec::new();
        for (_, imported) in &uses {
            for (path, local) in imported {
                if local == "*" || local == "_" {
                    continue;
                }
                for (pattern, origin) in &all {
                    if let Some(rest) = pattern.strip_prefix(path.as_str())
                        && rest.starts_with("::")
                    {
                        let alias = format!("{local}{rest}");
                        if known.insert(alias.clone()) {
                            // Am Deckel: nicht beurteilbar, nie still PASS.
                            if known.len() > MAX_USE_PATHS {
                                return Scan::Unreadable(
                                    "more import aliases than the scanner follows",
                                );
                            }
                            derived.push((alias, origin.clone()));
                        }
                    }
                }
            }
        }
        if derived.is_empty() {
            settled = true;
            break;
        }
        all.extend(derived);
    }
    if !settled {
        return Scan::Unreadable("a chain of import aliases longer than the scanner follows");
    }
    for (start, imported) in &uses {
        for (path, _) in imported {
            for (pattern, origin) in &all {
                if path == pattern || path.starts_with(&format!("{pattern}::")) {
                    return Scan::Hit {
                        line: line_of(&normal, *start),
                        pattern: origin.clone(),
                    };
                }
                if let Some(prefix) = path.strip_suffix("::*")
                    && pattern.starts_with(&format!("{prefix}::"))
                {
                    return Scan::Unreadable("a glob import may bring a forbidden path into scope");
                }
            }
        }
    }
    // Der Pfad irgendwo im Code.
    for (pattern, origin) in &all {
        if let Some(at) = path_at(&normal, pattern) {
            return Scan::Hit {
                line: line_of(&normal, at),
                pattern: origin.clone(),
            };
        }
    }
    Scan::Clean
}

/// Blendet Kommentare, String- und Zeichenliterale aus (durch Leerzeichen,
/// Zeilenumbrüche bleiben — so stimmen die Zeilennummern).
fn blank(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let keep = |c: char| if c == '\n' { '\n' } else { ' ' };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(keep(chars[i]));
                    i += 1;
                }
            }
            continue;
        }
        // Rohe Strings: r"…", r#"…"#, br"…", cr"…".
        if (c == 'r' || (matches!(c, 'b' | 'c') && next == Some('r')))
            && !chars[..i]
                .last()
                .is_some_and(|p| p.is_alphanumeric() || *p == '_')
        {
            let mut j = i + if c == 'r' { 1 } else { 2 };
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                let mut k = j + 1;
                while k < chars.len() && !chars[k..].starts_with(&close) {
                    k += 1;
                }
                let end = (k + close.len()).min(chars.len());
                for &ch in &chars[i..end] {
                    out.push(keep(ch));
                }
                i = end;
                continue;
            }
        }
        if c == '"' {
            out.push(' ');
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(keep(chars[i]));
                    i += 1;
                }
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '\'' {
            // Zeichenliteral ('a', '\n', '\u{1F600}') — sonst eine Lifetime.
            // Nach `\` ist das nächste Zeichen maskiert (`'\''`).
            let close = if next == Some('\\') {
                chars
                    .get(i + 3..)
                    .unwrap_or_default()
                    .iter()
                    .take(12)
                    .position(|&ch| ch == '\'')
                    .map(|p| i + 3 + p)
            } else {
                (chars.get(i + 2) == Some(&'\'')).then_some(i + 2)
            };
            if let Some(end) = close {
                for _ in i..=end {
                    out.push(' ');
                }
                i = end + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Fasst Leerraum zusammen und entfernt ihn um `::`, `{`, `}` und `,` —
/// `crate :: evidence` ist `crate::evidence`. Zeilenumbrüche innerhalb von
/// Leerraum bleiben als einer erhalten (für die Zeilennummer).
fn normalize(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut pending: Option<char> = None;
    for c in code.chars() {
        if c.is_whitespace() {
            pending = Some(match pending {
                Some('\n') => '\n',
                _ if c == '\n' => '\n',
                _ => ' ',
            });
            continue;
        }
        if let Some(space) = pending.take() {
            let glue_before = out.ends_with("::")
                || out.ends_with('{')
                || out.ends_with(',')
                || out.ends_with('}');
            let glue_after = matches!(c, ':' | '{' | '}' | ',' | ';');
            if !(glue_before || glue_after) || space == '\n' {
                // Ein Zeilenumbruch bleibt — auch an Klebestellen, dann ohne
                // Wirkung auf die Pfade (siehe `flatten`, das ihn entfernt).
                out.push(space);
            }
        }
        out.push(c);
    }
    out
}

/// Löst einen `use`-Baum in volle Pfade auf. `None`: nicht lesbar.
/// Ein importierter Pfad und der Name, unter dem er lokal gilt (`as`, sonst
/// das letzte Segment; `*` bei einem Glob-Import).
type Imported = (String, String);

fn flatten(prefix: &str, tree: &str, depth: usize, out: &mut Vec<Imported>) -> Option<()> {
    if depth > MAX_USE_DEPTH || out.len() > MAX_USE_PATHS {
        return None;
    }
    let tree: String = tree.chars().filter(|c| *c != '\n').collect();
    let tree = tree.trim();
    let tree = tree.strip_prefix("::").unwrap_or(tree);
    let join = |head: &str| {
        if prefix.is_empty() {
            head.to_owned()
        } else {
            format!("{prefix}::{head}")
        }
    };
    if let Some(open) = tree.find('{') {
        let head = tree[..open].trim_end_matches("::");
        let inner = tree[open + 1..].strip_suffix('}')?;
        if !head.is_empty() && !valid_path(head) {
            return None;
        }
        let base = if head.is_empty() {
            prefix.to_owned()
        } else {
            join(head)
        };
        for item in split_top(inner)? {
            if item.trim().is_empty() {
                continue;
            }
            flatten(&base, item, depth + 1, out)?;
        }
        return Some(());
    }
    // `pfad`, `pfad as name`, `pfad::*`, `self`.
    let mut parts = tree.split(" as ");
    let path = parts.next()?.trim();
    let alias = parts.next().map(str::trim);
    if parts.next().is_some() || alias.is_some_and(|a| !valid_path(a) || a.contains("::")) {
        return None;
    }
    let path = path.strip_suffix("::self").unwrap_or(path);
    let full = if path == "self" {
        prefix.to_owned()
    } else if path == "*" {
        out.push((join("*"), "*".to_owned()));
        return Some(());
    } else if let Some(head) = path.strip_suffix("::*") {
        valid_path(head).then_some(())?;
        out.push((format!("{}::*", join(head)), "*".to_owned()));
        return Some(());
    } else {
        valid_path(path).then_some(())?;
        join(path)
    };
    if full.is_empty() {
        return None;
    }
    let local = alias
        .map(str::to_owned)
        .unwrap_or_else(|| full.rsplit("::").next().unwrap_or(&full).to_owned());
    out.push((full, local));
    Some(())
}

/// Teilt eine Gruppe an den Kommas der obersten Ebene.
fn split_top(inner: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then(|| {
        parts.push(&inner[start..]);
        parts
    })
}

/// Bezeichner, getrennt durch `::`.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split("::").all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '#')
        })
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Das Schlüsselwort `word` als ganzes Wort, gefolgt von Leerraum, `::`
/// oder `{`.
fn find_word(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = text[from..].find(word) {
        let start = from + offset;
        let end = start + word.len();
        let before = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident(c));
        let after = text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == ':' || c == '{');
        if before && after {
            return Some(start);
        }
        from = end;
    }
    None
}

/// `marker` mit nicht-Bezeichner davor.
fn bounded_find(text: &str, marker: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = text[from..].find(marker) {
        let start = from + offset;
        if text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident(c))
        {
            return true;
        }
        from = start + marker.len();
    }
    false
}

/// `pattern` als ganze Bezeichnerfolge im Code: davor kein Bezeichner und
/// kein `::` eines anderen Pfads (ein führendes `::` zählt), danach kein
/// Bezeichnerzeichen.
fn path_at(text: &str, pattern: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = text[from..].find(pattern) {
        let start = from + offset;
        let end = start + pattern.len();
        let before = &text[..start];
        // Davor nichts Bezeichnerhaftes, ein führendes `::` oder einer der
        // Pfadanfänge `self::`, `super::`, `crate::` (der Pfad relativ zum
        // eigenen Modul — im Zweifel ein Treffer, nie still übersehen).
        let before_ok = match before.strip_suffix("::") {
            Some(rest) => {
                let head = rest
                    .rsplit(|c: char| !is_ident(c))
                    .next()
                    .unwrap_or_default();
                head.is_empty() || matches!(head, "self" | "super" | "crate")
            }
            None => before.chars().next_back().is_none_or(|c| !is_ident(c)),
        };
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
        while !text.is_char_boundary(from) {
            from += 1;
        }
    }
    None
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::{Scan, scan};

    fn hit(source: &str, pattern: &str) -> bool {
        matches!(scan(source, &[pattern.to_owned()]), Scan::Hit { .. })
    }

    #[test]
    fn matches_whole_paths_only() {
        assert!(hit("use minds_reader::Index;", "minds_reader"));
        assert!(hit("let x = minds_reader::query();", "minds_reader"));
        assert!(hit("extern crate minds_reader;", "minds_reader"));
        assert!(hit("use ::minds_reader::X;", "minds_reader"));
        assert!(hit("let x = ::minds_reader::f();", "minds_reader"));
        assert!(!hit("use my_minds_reader::Index;", "minds_reader"));
        assert!(!hit("use minds_reader_ext::Index;", "minds_reader"));
        assert!(!hit("use other::minds_reader::x;", "minds_reader"));
        assert!(hit("use crate::evidence::Seal;", "crate::evidence"));
        assert!(!hit("use crate::evidence_old::Seal;", "crate::evidence"));
        assert!(hit("let s = crate :: evidence :: Seal;", "crate::evidence"));
    }

    #[test]
    fn sees_grouped_and_multiline_imports() {
        assert!(hit("use crate::{index, evidence};", "crate::evidence"));
        assert!(hit("use crate::{evidence::Seal, x};", "crate::evidence"));
        assert!(hit("use crate::{evidence as e};", "crate::evidence"));
        assert!(hit(
            "use crate::{index::{A, B}, evidence};",
            "crate::evidence"
        ));
        assert!(hit(
            "use crate::{\n    index,\n    evidence,\n};",
            "crate::evidence"
        ));
        assert!(hit(
            "use crate::{\n    index::{\n        A,\n    },\n    evidence::{self, Seal},\n};",
            "crate::evidence"
        ));
        assert!(!hit("use crate::{evidences, x};", "crate::evidence"));
        assert!(!hit("use other_crate::{evidence};", "crate::evidence"));
        let lines = scan(
            "// x\nuse a;\nuse crate::{\n  evidence,\n};",
            &["crate::evidence".into()],
        );
        assert_eq!(
            lines,
            Scan::Hit {
                line: 3,
                pattern: "crate::evidence".into()
            }
        );
    }

    /// Kommentare und Strings zählen nicht.
    #[test]
    fn comments_and_strings_do_not_count() {
        assert!(!hit("// use minds_reader::x;", "minds_reader"));
        assert!(!hit("/* use minds_reader::x; */ fn a() {}", "minds_reader"));
        assert!(!hit("let s = \"minds_reader::x\";", "minds_reader"));
        assert!(!hit("let s = r#\"use minds_reader::x;\"#;", "minds_reader"));
        assert!(!hit("let c = '\"'; let s = \"a\";", "minds_reader"));
        assert!(hit("let c = '\"'; use minds_reader::x;", "minds_reader"));
        assert!(hit(
            "fn f<'a>(x: &'a str) { minds_reader::g(x) }",
            "minds_reader"
        ));
    }

    /// Ein Präfix importiert, der Rest im Code: derselbe verbotene Pfad.
    #[test]
    fn prefix_imports_and_aliases_are_followed() {
        assert!(hit(
            "use crate::evidence;\nfn f() { evidence::store::put() }",
            "crate::evidence::store"
        ));
        assert!(hit(
            "use minds_reader::{self as r};\nfn f() { r::conformance::evaluate() }",
            "minds_reader::conformance"
        ));
        assert!(hit(
            "use crate::evidence as ev;\nuse ev::store;",
            "crate::evidence::store"
        ));
        assert!(!hit(
            "use crate::evidence;\nfn f() { evidence::other() }",
            "crate::evidence::store"
        ));
        // Ein roher C-String endet am ersten `"` — `\` maskiert darin nichts.
        assert!(hit(
            r#"let _ = cr"\"; use minds_reader::x; //";"#,
            "minds_reader"
        ));
    }

    /// An den Deckeln: nicht beurteilbar — nie still PASS.
    #[test]
    fn caps_are_unreadable_not_clean() {
        let mut many = String::new();
        for i in 0..5000 {
            many.push_str(&format!("use db as a{i};\n"));
        }
        many.push_str("fn f() { a0::Pool::new(); }\n");
        assert!(matches!(
            scan(&many, &["db::Pool".into()]),
            Scan::Unreadable(_)
        ));
        let mut chain = String::from("use db as a0;\n");
        for i in 1..12 {
            chain.push_str(&format!("use a{} as a{i};\n", i - 1));
        }
        chain.push_str("fn f() { a11::Pool::x(); }\n");
        assert!(!matches!(scan(&chain, &["db::Pool".into()]), Scan::Clean));
        let short = "use db as a;\nuse a as b;\nuse b as c;\nfn f() { c::Pool::x(); }";
        assert!(matches!(
            scan(short, &["db::Pool".into()]),
            Scan::Hit { .. }
        ));
        assert!(hit(
            "use db;\nfn f() { self::db::Pool::new(); }",
            "db::Pool"
        ));
    }

    /// Was der Scan nicht beurteilen kann: UNKNOWN.
    #[test]
    fn evasions_are_unreadable() {
        for source in [
            "#[path = \"../outside.rs\"]\nmod x;",
            "include!(\"../x.rs\");",
            "use crate::*;",
            "use crate::{a, b",
        ] {
            assert!(
                matches!(
                    scan(source, &["crate::evidence".into()]),
                    Scan::Unreadable(_)
                ),
                "{source}"
            );
        }
        assert_eq!(
            scan("my_include!(x);", &["crate::evidence".into()]),
            Scan::Clean
        );
    }
}
