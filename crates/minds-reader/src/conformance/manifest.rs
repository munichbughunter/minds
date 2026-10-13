//! Architekturregel `no-dependency`: Abhängigkeiten zwischen Crates aus den
//! `Cargo.toml` eines Commits — ohne TOML-Abhängigkeit, mit einem bewusst
//! kleinen, zeilenbasierten Teil von TOML.
//!
//! Gelesen wird, was Cargo-Manifeste für Abhängigkeiten tatsächlich
//! benutzen: Abschnittsköpfe (`[dependencies]`, `[build-dependencies]`,
//! `[target.'…'.dependencies]`, `[dependencies.name]`,
//! `[workspace.dependencies]`), Schlüssel `name = …` / `"name" = …` /
//! `name.workspace = true` / `name.package = "…"` und Inline-Tabellen mit
//! `package` und `workspace`. Umbenennungen werden aufgelöst — auch über
//! `[workspace.dependencies]`: `x.workspace = true` hängt von dem Paket ab,
//! das der Workspace unter `x` nennt.
//!
//! Was der Teil nicht sicher versteht, macht das Manifest **unlesbar** und
//! die Regel UNKNOWN statt PASS: mehrzeilige Strings, Inline-Tabellen über
//! mehrere Zeilen in einem Abhängigkeitsabschnitt, Abhängigkeiten als
//! gepunktete Schlüssel außerhalb ihres Abschnitts (`dependencies.x = …`,
//! `package.name = …`), ein Manifest ohne `[package]` und ohne
//! `[workspace]`, ein `Cargo.toml`, das ein Symlink ist.
//! `[dev-dependencies]` zählen nicht: Testcode ist keine Architektur.

use std::collections::{BTreeMap, BTreeSet};

use super::{Judged, MAX_MANIFEST, Outcome, Snapshot, path_to};
use crate::text::sanitize_path;

/// Höchstzahl der gelesenen Manifeste.
const MAX_MANIFESTS: usize = 1_000;

/// Eine Abhängigkeit, wie das Manifest sie nennt.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Dep {
    /// Der Schlüssel (so heißt die Crate im Code, `-` als `_`).
    pub alias: String,
    /// `package = "…"`: das echte Paket.
    pub package: Option<String>,
    /// `workspace = true`: aus `[workspace.dependencies]` geerbt.
    pub workspace: bool,
}

/// Ein gelesenes Manifest.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Manifest {
    /// `[package] name`.
    pub name: Option<String>,
    /// Ob es ein `[workspace]` trägt.
    pub is_workspace: bool,
    /// Die Abhängigkeiten (ohne `[dev-dependencies]`).
    pub deps: Vec<Dep>,
    /// `[workspace.dependencies]`.
    pub workspace_deps: Vec<Dep>,
}

/// Die Abhängigkeitskanten aller Pakete eines Commits, Umbenennungen
/// aufgelöst; dazu, welche Aliasse auf welches Paket zeigen.
pub(crate) struct Graph {
    pub edges: BTreeMap<String, BTreeSet<String>>,
    /// Paket → Aliasse, unter denen Code es nennt (als Rust-Bezeichner).
    pub aliases: BTreeMap<String, BTreeSet<String>>,
    /// Was nicht lesbar war (Pfade, entschärft).
    pub unreadable: Vec<String>,
}

/// Liest alle Manifeste des Commits.
pub(crate) fn graph(snapshot: &Snapshot<'_>) -> Result<Graph, String> {
    let is_manifest = |path: &str| path == "Cargo.toml" || path.ends_with("/Cargo.toml");
    if let Some(link) = snapshot.links()?.iter().find(|link| is_manifest(link)) {
        return Err(format!(
            "{} is a symlink — the dependency graph is not checked",
            sanitize_path(link)
        ));
    }
    let manifests: Vec<_> = snapshot
        .files()?
        .iter()
        .filter(|(path, _)| is_manifest(path))
        .collect();
    if manifests.len() > MAX_MANIFESTS {
        return Err(format!("more than {MAX_MANIFESTS} Cargo.toml files"));
    }
    let mut unreadable = Vec::new();
    let mut packages: Vec<(String, Vec<Dep>)> = Vec::new();
    let mut workspace: BTreeMap<String, Dep> = BTreeMap::new();
    for (path, blob) in manifests {
        let parsed = snapshot
            .read(path, *blob, MAX_MANIFEST)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| parse(&text));
        match parsed {
            Some(manifest) => {
                for dep in manifest.workspace_deps {
                    workspace.insert(dep.alias.clone(), dep);
                }
                if let Some(name) = manifest.name {
                    packages.push((name, manifest.deps));
                }
            }
            None => unreadable.push(sanitize_path(path)),
        }
    }
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, deps) in packages {
        let entry = edges.entry(name.clone()).or_default();
        for dep in deps {
            let real = if dep.workspace {
                match workspace.get(&dep.alias) {
                    Some(inherited) => inherited
                        .package
                        .clone()
                        .unwrap_or_else(|| inherited.alias.clone()),
                    None => {
                        unreadable.push(format!(
                            "{} (workspace dependency {} not declared)",
                            sanitize_path(&name),
                            sanitize_path(&dep.alias)
                        ));
                        continue;
                    }
                }
            } else {
                dep.package.clone().unwrap_or_else(|| dep.alias.clone())
            };
            aliases
                .entry(real.clone())
                .or_default()
                .insert(dep.alias.replace('-', "_"));
            entry.insert(real);
        }
    }
    Ok(Graph {
        edges,
        aliases,
        unreadable,
    })
}

pub(super) fn no_dependency(snapshot: &Snapshot<'_>, krate: &str, on: &[String]) -> Judged {
    let Graph {
        edges, unreadable, ..
    } = match graph(snapshot) {
        Ok(graph) => graph,
        Err(reason) => return (Outcome::Unknown, reason),
    };
    if let Some(path) = path_to(&edges, krate, on) {
        return (Outcome::Drift, path.join(" → "));
    }
    if let Some(what) = unreadable.first() {
        return (
            Outcome::Unknown,
            format!(
                "no path found, but {what} is not readable ({} unreadable)",
                unreadable.len()
            ),
        );
    }
    if !edges.contains_key(krate) {
        return (
            Outcome::Unknown,
            format!("no crate named {krate} in this commit"),
        );
    }
    (
        Outcome::Pass,
        format!("no dependency path ({} crates checked)", edges.len()),
    )
}

/// Wo im Manifest eine Zeile steht.
enum Section {
    /// Vor dem ersten Kopf.
    Root,
    Package,
    Workspace,
    /// `[dependencies]`, `[build-dependencies]` und ihre `target`-Formen —
    /// oder (`workspace = true`) `[workspace.dependencies]`.
    Deps {
        workspace: bool,
    },
    /// `[dependencies.name]`: Die Abhängigkeit ist der Abschnitt selbst.
    Dep {
        index: usize,
        workspace: bool,
    },
    /// Ein Abschnitt ohne Abhängigkeiten.
    Other,
}

/// Liest ein Manifest. `None`, wenn es den verstandenen Teil verlässt.
pub(crate) fn parse(text: &str) -> Option<Manifest> {
    let mut manifest = Manifest::default();
    let mut section = Section::Root;
    let mut has_package = false;
    // Offene Klammern eines mehrzeiligen Werts (Arrays in anderen
    // Abschnitten, etwa `features = [ … ]`).
    let mut depth: i32 = 0;
    for raw in text.lines() {
        if raw.contains("\"\"\"") || raw.contains("'''") {
            return None;
        }
        let line = strip_comment(raw)?.trim();
        if line.is_empty() {
            continue;
        }
        if depth > 0 {
            depth += bracket_balance(line)?;
            if depth < 0 {
                return None;
            }
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let header = header
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix("]]"))
                .or_else(|| header.strip_suffix(']'))?
                .trim();
            section = classify(header, &mut manifest)?;
            if matches!(section, Section::Package) {
                has_package = true;
            }
            if matches!(section, Section::Workspace) {
                manifest.is_workspace = true;
            }
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = split_header(key.trim())?;
        let value = value.trim();
        let first = key.first()?.as_str();
        match &section {
            Section::Root | Section::Other | Section::Workspace | Section::Package
                if key.iter().any(|segment| {
                    segment == "dependencies" || segment == "build-dependencies"
                }) || matches!(
                    (first, &section),
                    ("target", _) | ("package" | "workspace", Section::Root)
                ) =>
            {
                // Abhängigkeiten oder der Paketname als gepunkteter Schlüssel
                // außerhalb ihres Abschnitts (auch unter einem nackten
                // `[target]`) — nicht gelesen, also unlesbar.
                return None;
            }
            Section::Package => {
                if key.len() == 1 && first == "name" {
                    manifest.name = Some(unquote(value)?.to_owned());
                }
                depth += bracket_balance(value)?;
            }
            Section::Deps { workspace } => {
                let alias = valid_key(first)?.to_owned();
                if value.starts_with('[') {
                    return None;
                }
                if value.starts_with('{') && !value.ends_with('}') {
                    return None; // Inline-Tabelle über mehrere Zeilen
                }
                let list = if *workspace {
                    &mut manifest.workspace_deps
                } else {
                    &mut manifest.deps
                };
                let index = match list.iter().position(|dep| dep.alias == alias) {
                    Some(index) => index,
                    None => {
                        list.push(Dep {
                            alias,
                            ..Dep::default()
                        });
                        list.len() - 1
                    }
                };
                let dep = &mut list[index];
                match key.get(1).map(String::as_str) {
                    None => {
                        if let Some(table) = inline_table(value) {
                            apply(dep, &table)?;
                        }
                    }
                    Some("package") if key.len() == 2 => {
                        dep.package = Some(unquote(value)?.to_owned());
                    }
                    Some("workspace") if key.len() == 2 => dep.workspace = value == "true",
                    Some(_) => {}
                }
            }
            Section::Dep { index, workspace } => {
                let list = if *workspace {
                    &mut manifest.workspace_deps
                } else {
                    &mut manifest.deps
                };
                let dep = list.get_mut(*index)?;
                match (key.len(), first) {
                    (1, "package") => dep.package = Some(unquote(value)?.to_owned()),
                    (1, "workspace") => dep.workspace = value == "true",
                    _ => {}
                }
                depth += bracket_balance(value)?;
            }
            Section::Root | Section::Other | Section::Workspace => {
                depth += bracket_balance(value)?;
            }
        }
        if depth < 0 {
            return None;
        }
    }
    // Ein Manifest ohne Paket und ohne Workspace ist kein verstandenes.
    // Ein Paket ohne lesbaren Namen ebenso wenig.
    (depth == 0
        && (has_package || manifest.is_workspace)
        && (!has_package || manifest.name.is_some()))
    .then_some(manifest)
}

/// Ordnet einen Abschnittskopf ein (und legt `[dependencies.name]` an).
fn classify(header: &str, manifest: &mut Manifest) -> Option<Section> {
    let parts = split_header(header)?;
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let deps = |kind: &str| kind == "dependencies" || kind == "build-dependencies";
    let mut table = |name: &str, workspace: bool| {
        let alias = valid_key(name)?.to_owned();
        let list = if workspace {
            &mut manifest.workspace_deps
        } else {
            &mut manifest.deps
        };
        let index = list
            .iter()
            .position(|dep| dep.alias == alias)
            .unwrap_or_else(|| {
                list.push(Dep {
                    alias,
                    ..Dep::default()
                });
                list.len() - 1
            });
        Some(Section::Dep { index, workspace })
    };
    Some(match parts.as_slice() {
        ["package"] => Section::Package,
        ["workspace"] => Section::Workspace,
        ["workspace", "dependencies"] => Section::Deps { workspace: true },
        ["workspace", "dependencies", name] => table(name, true)?,
        [kind] if deps(kind) => Section::Deps { workspace: false },
        [kind, name] if deps(kind) => table(name, false)?,
        ["target", _, kind] if deps(kind) => Section::Deps { workspace: false },
        ["target", _, kind, name] if deps(kind) => table(name, false)?,
        // Tiefer darunter (`[dependencies.x.features]`): nicht verstanden.
        [kind, ..] if deps(kind) => return None,
        ["target", _, kind, ..] if deps(kind) => return None,
        _ => Section::Other,
    })
}

/// Die Paare einer einzeiligen Inline-Tabelle `{ a = 1, b = "x" }` —
/// `None`, wenn der Wert keine ist.
fn inline_table(value: &str) -> Option<Vec<(String, String)>> {
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut pairs = Vec::new();
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    for c in inner.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(c);
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                current.push(c);
            }
            (None, '[' | '{') => {
                depth += 1;
                current.push(c);
            }
            (None, ']' | '}') => {
                depth -= 1;
                current.push(c);
            }
            (None, ',') if depth == 0 => pairs.push(std::mem::take(&mut current)),
            (None, c) => current.push(c),
        }
    }
    pairs.push(current);
    Some(
        pairs
            .iter()
            .filter(|pair| !pair.trim().is_empty())
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                Some((key.trim().to_owned(), value.trim().to_owned()))
            })
            .collect(),
    )
}

/// Übernimmt `package` und `workspace` aus einer Inline-Tabelle. Ein
/// gepunkteter Schlüssel darin (`package.x`) ist nicht verstanden.
fn apply(dep: &mut Dep, table: &[(String, String)]) -> Option<()> {
    for (key, value) in table {
        match key.as_str() {
            "package" => dep.package = Some(unquote(value)?.to_owned()),
            "workspace" => dep.workspace = value == "true",
            key if key.contains('.') => return None,
            _ => {}
        }
    }
    Some(())
}

/// Zerlegt `target.'cfg(unix)'.dependencies` in seine Teile — Punkte in
/// Anführungszeichen trennen nicht.
fn split_header(header: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in header.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, '.') => parts.push(std::mem::take(&mut current).trim().to_owned()),
            (None, c) => current.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(current.trim().to_owned());
    Some(parts)
}

/// Schneidet einen Kommentar ab (`#` außerhalb von Strings).
fn strip_comment(line: &str) -> Option<&str> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, c) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return Some(&line[..index]),
            None => {}
        }
    }
    if quote.is_some() {
        return None; // offener String am Zeilenende
    }
    Some(line)
}

/// Saldo von `[`/`]` und `{`/`}` außerhalb von Strings.
fn bracket_balance(value: &str) -> Option<i32> {
    let mut balance = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in value.chars() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' | '{' => balance += 1,
                ']' | '}' => balance -= 1,
                _ => {}
            },
        }
    }
    quote.is_none().then_some(balance)
}

/// Ein einfacher String-Wert `"x"` oder `'x'`.
fn unquote(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .filter(|v| !v.contains(['"', '\\']))
}

/// Ein Paket- oder Alias-Name.
fn valid_key(key: &str) -> Option<&str> {
    (!key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    .then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps(manifest: &Manifest) -> Vec<(String, Option<String>, bool)> {
        manifest
            .deps
            .iter()
            .map(|dep| (dep.alias.clone(), dep.package.clone(), dep.workspace))
            .collect()
    }

    fn dep(alias: &str, package: Option<&str>, workspace: bool) -> (String, Option<String>, bool) {
        (alias.into(), package.map(Into::into), workspace)
    }

    #[test]
    fn reads_the_forms_this_workspace_uses() {
        let manifest = parse(
            r#"
[package]
name = "minds-cli" # Kommentar
version = "0.1.0"
keywords = [
    "git",
    "[nicht ein Kopf]",
]

[dependencies]
minds-core = { path = "../minds-core" }
minds-store = { version = "0.1.0", path = "../minds-store" }
serde.workspace = true
"quoted" = "1"
alias = { package = "real-name", version = "1" }
dotted.package = "other-real"
dotted.path = "../x"
minds-tui = { path = "../minds-tui", optional = true }

[target.'cfg(unix)'.dependencies]
libc = { workspace = true }

[dependencies.extra]
version = "1"

[build-dependencies]
cc = "1"

[dev-dependencies]
tempfile = "3"

[features]
default = ["dep:minds-tui"]
"#,
        )
        .unwrap();
        assert_eq!(manifest.name.as_deref(), Some("minds-cli"));
        assert_eq!(
            deps(&manifest),
            vec![
                dep("minds-core", None, false),
                dep("minds-store", None, false),
                dep("serde", None, true),
                dep("quoted", None, false),
                dep("alias", Some("real-name"), false),
                dep("dotted", Some("other-real"), false),
                dep("minds-tui", None, false),
                dep("libc", None, true),
                dep("extra", None, false),
                dep("cc", None, false),
            ]
        );
    }

    #[test]
    fn renamed_table_dependency() {
        let manifest =
            parse("[package]\nname = \"a\"\n[dependencies.b]\npackage = \"c\"\n").unwrap();
        assert_eq!(deps(&manifest), vec![dep("b", Some("c"), false)]);
    }

    #[test]
    fn workspace_dependencies_are_read() {
        let manifest = parse(
            "[workspace]\nmembers = [\"crates/*\"]\n[workspace.dependencies]\ninnocent = { package = \"minds-cli\", path = \"x\" }\nserde = \"1\"\n",
        )
        .unwrap();
        assert!(manifest.is_workspace);
        assert_eq!(manifest.name, None);
        assert_eq!(
            manifest.workspace_deps[0].package.as_deref(),
            Some("minds-cli")
        );
    }

    /// Was der Teil nicht versteht, ist unlesbar — nie still leer.
    #[test]
    fn refuses_what_it_does_not_understand() {
        for text in [
            "[package]\nname = \"a\"\n[dependencies]\nb = { path = \"x\",\n  version = \"1\" }\n",
            "[package]\ndescription = \"\"\"\nmehr\n\"\"\"\n",
            "[package]\nname = \"a\n",
            "[package\nname = \"a\"\n",
            "[package]\nkeywords = [\n\"a\",\n",
            // Abhängigkeiten als gepunktete Schlüssel außerhalb ihres Abschnitts.
            "package.name = \"a\"\n",
            "dependencies.b = { path = \"..\" }\n[package]\nname = \"a\"\n",
            "[package]\nname = \"a\"\n[target.'cfg(unix)']\ndependencies.b.path = \"..\"\n",
            "[package]\nname = \"a\"\ndependencies = { b = { path = \"..\" } }\n",
            "[package]\nname = \"a\"\n[dependencies.b.features]\nx = 1\n",
            "[package]\nname = \"a\"\n[target]\n'cfg(unix)'.dependencies.b = { path = \"..\" }\n",
            // Weder Paket noch Workspace.
            "[lib]\npath = \"x.rs\"\n",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
