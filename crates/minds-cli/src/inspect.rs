//! `minds inspect [<suche> | <datei>:<zeile>]` — die Oberfläche über dem
//! Kontext. Öffnet Repo, Store und Review-Store und reicht sie an
//! `minds-tui`; die Oberfläche selbst fasst kein Git an.

use std::process::ExitCode;

use minds_git::Repo;
use minds_reader::Inspection;
use minds_store::ReviewStore;
use minds_tui::{Options, Start};

use crate::context::Context;

type Fallible<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Führt `minds inspect` aus. Das Positional ist entweder `<datei>:<zeile>`
/// (dann beginnt die Oberfläche bei der Why-Kette) oder ein Suchbegriff.
pub fn run(target: Option<&str>) -> ExitCode {
    match inspect(target) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minds inspect: {err}");
            ExitCode::FAILURE
        }
    }
}

fn inspect(target: Option<&str>) -> Fallible<()> {
    let ctx = Context::open()?;
    let reviews = ReviewStore::new(Repo::open(&ctx.root)?);
    let name = ctx
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ctx.root.display().to_string());
    let inspection = Inspection::load(&ctx.repo, ctx.store.as_ref(), Some(&reviews), &name)?;
    let opts = match target.and_then(split) {
        Some((path, line)) => Options {
            query: None,
            start: Start::Why {
                path: path.to_string(),
                line,
            },
        },
        None => Options {
            query: target.map(str::to_string),
            start: Start::Activity,
        },
    };
    minds_tui::run(inspection, &ctx.repo, opts)?;
    Ok(())
}

/// `<datei>:<zeile>` — wie bei `why`, nur dass ein Nichttreffer hier kein
/// Fehler ist, sondern ein Suchbegriff.
fn split(target: &str) -> Option<(&str, u32)> {
    let (path, line) = target.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    if path.is_empty() || line == 0 {
        return None;
    }
    Some((path, line))
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_file_line_target_is_split_anything_else_is_a_query() {
        assert_eq!(split("src/retry.rs:42"), Some(("src/retry.rs", 42)));
        assert_eq!(split("weird:name.rs:7"), Some(("weird:name.rs", 7)));
        assert_eq!(split("retry"), None);
        assert_eq!(split("src/retry.rs:0"), None);
        assert_eq!(split(":42"), None);
        assert_eq!(split("glpat:rotation"), None);
    }
}
