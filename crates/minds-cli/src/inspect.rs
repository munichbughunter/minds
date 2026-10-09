//! `minds inspect [<suche> | <datei>:<zeile>]` — die Oberfläche über dem
//! Kontext. Öffnet Repo, Store und Review-Store und reicht sie an
//! `minds-tui`; die Oberfläche selbst fasst kein Git an.
//!
//! Live: Die Oberfläche fragt [`Live`] jede Sekunde nach einem
//! Fingerabdruck — HEAD plus die Spitzen aller Refs unter `refs/minds/` im
//! Code-Repository (Reviews, In-Repo-Store) und im Repository des Stores
//! (Child-Repo). Ändert er sich, wird über denselben Weg wie beim Start neu
//! geladen. Alles in-process über `gix`; kein `git`-Unterprozess.

use std::fmt::Write as _;
use std::process::ExitCode;

use minds_git::{Head, MINDS_REF_NAMESPACE, Repo};
use minds_reader::Inspection;
use minds_store::{ContextStore, ReviewStore};
use minds_tui::{Options, Source, Stamp, Start};

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
    let live = Live {
        ctx: &ctx,
        reviews: &reviews,
        name: &name,
    };
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
    minds_tui::run(&live, &ctx.repo, opts)?;
    Ok(())
}

/// Die Quelle der Oberfläche: dieselben geöffneten Handles wie beim Start.
/// Ein offenes `gix`-Repository liest lose Refs, geänderte `packed-refs` und
/// neue Objekte bei jedem Zugriff frisch — erneutes Öffnen ist unnötig.
struct Live<'a> {
    ctx: &'a Context,
    reviews: &'a ReviewStore,
    name: &'a str,
}

impl Source for Live<'_> {
    fn load(&self) -> minds_reader::Result<Inspection> {
        Inspection::load(
            &self.ctx.repo,
            self.ctx.store.as_ref(),
            Some(self.reviews),
            self.name,
        )
    }

    fn stamp(&self) -> Option<Stamp> {
        stamp(&self.ctx.repo, self.ctx.store.as_ref())
    }
}

/// HEAD (Branch und Commit) für sich und die Spitzen unter `refs/minds/` in
/// Code- und Store-Repository als BLAKE3-Hash — klein, auch bei sehr vielen
/// Refs. `None`, sobald etwas davon nicht lesbar ist — dann lädt die
/// Oberfläche nicht von selbst neu.
fn stamp(repo: &Repo, store: &dyn ContextStore) -> Option<Stamp> {
    let head = match repo.head().ok()? {
        Head::Branch { name, commit } => format!("{name} {commit}"),
        Head::Unborn { name } => format!("{name} unborn"),
        Head::Detached { commit } => format!("detached {commit}"),
    };
    let mut refs = blake3::Hasher::new();
    let mut line = String::new();
    for (name, id) in repo.refs_under(MINDS_REF_NAMESPACE).ok()? {
        line.clear();
        let _ = writeln!(line, "{name} {id}");
        refs.update(line.as_bytes());
    }
    // Beim In-Repo-Store stehen dieselben Refs hier ein zweites Mal —
    // redundant, aber harmlos; so braucht der Abdruck keine Backend-Weiche.
    refs.update(b"store\n");
    // Ein Store, der seinen Stand nicht billig nennen kann (`None`), macht
    // den ganzen Abdruck unbestimmbar — „live off" statt vorgetäuschter
    // Frische.
    for (name, id) in store.tips().ok()?? {
        line.clear();
        let _ = writeln!(line, "{name} {id}");
        refs.update(line.as_bytes());
    }
    Some(Stamp {
        head,
        refs: refs.finalize().to_hex().to_string(),
    })
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
    use std::path::Path;
    use std::process::Command;

    use minds_core::{Agent, Intent, Model, Session};
    use minds_git::Repo;
    use minds_store::{ChildRepoStore, ContextStore, InRepoStore, ReviewStore};
    use minds_tui::Source;

    use super::{Live, split, stamp};
    use crate::context::Context;

    /// `git` ohne die Konfiguration des Rechners — ein globales
    /// `commit.gpgsign` hielte sonst den Test an.
    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    /// Ein Repo mit Identität (Store-Commits brauchen eine) und einem Commit.
    fn code_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(
            dir.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "code"]);
        dir
    }

    #[test]
    fn the_stamp_follows_head_the_minds_refs_and_the_store() {
        let dir = code_repo();
        let repo = Repo::open(dir.path()).unwrap();
        let store = InRepoStore::open(dir.path()).unwrap();

        let s0 = stamp(&repo, &store).expect("lesbar");
        assert_eq!(stamp(&repo, &store).unwrap(), s0, "ohne Änderung stabil");

        store.put_index_bytes(b"{}").unwrap();
        let s1 = stamp(&repo, &store).unwrap();
        assert_ne!(s1, s0, "Store geschrieben");

        // Reviews liegen im Code-Repository, auch neben einem Child-Store.
        git(
            dir.path(),
            &["update-ref", "refs/minds/reviews/probe", "HEAD"],
        );
        let s2 = stamp(&repo, &store).unwrap();
        assert_ne!(s2, s1, "Ref unter refs/minds/ neu");

        // Neuer Commit, Amend, Reset zurück: HEAD bewegt sich jedes Mal.
        git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "mehr"]);
        let s3 = stamp(&repo, &store).unwrap();
        assert_ne!(s3, s2, "neuer Commit");
        git(
            dir.path(),
            &["commit", "-q", "--amend", "--allow-empty", "-m", "geändert"],
        );
        let s4 = stamp(&repo, &store).unwrap();
        assert_ne!(s4, s3, "Amend");
        git(dir.path(), &["reset", "-q", "--soft", "HEAD~1"]);
        assert_eq!(
            stamp(&repo, &store).unwrap(),
            s2,
            "Reset auf den alten Stand"
        );
        git(dir.path(), &["checkout", "-q", "--detach"]);
        assert_ne!(stamp(&repo, &store).unwrap(), s2, "Detached HEAD");

        // Fremde Refs außerhalb von refs/minds/ lösen nichts aus.
        let s5 = stamp(&repo, &store).unwrap();
        git(dir.path(), &["update-ref", "refs/heads/nebenbei", "HEAD"]);
        assert_eq!(stamp(&repo, &store).unwrap(), s5);
    }

    /// Ein Store ohne eigenen Abdruck (Trait-Default) — nur, um zu prüfen,
    /// dass `stamp` dann „unbestimmbar" sagt.
    struct Blind;

    impl ContextStore for Blind {
        fn put_bytes(
            &self,
            _: &minds_store::SessionBytes,
        ) -> minds_store::Result<minds_store::Put> {
            unimplemented!()
        }
        fn get_bytes(&self, _: minds_core::SessionId) -> minds_store::Result<Option<Vec<u8>>> {
            unimplemented!()
        }
        fn list(&self) -> minds_store::Result<Vec<minds_core::SessionId>> {
            unimplemented!()
        }
        fn get_index_bytes(&self) -> minds_store::Result<Option<Vec<u8>>> {
            unimplemented!()
        }
        fn put_index_bytes(&self, _: &[u8]) -> minds_store::Result<()> {
            unimplemented!()
        }
        fn forget(
            &self,
            _: minds_core::SessionId,
            _: &str,
        ) -> minds_store::Result<minds_store::Forget> {
            unimplemented!()
        }
    }

    #[test]
    fn a_store_without_tips_makes_the_stamp_unknown() {
        let dir = code_repo();
        let repo = Repo::open(dir.path()).unwrap();
        assert_eq!(stamp(&repo, &Blind), None);
    }

    #[test]
    fn an_unborn_head_still_has_a_stamp() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let repo = Repo::open(dir.path()).unwrap();
        let store = InRepoStore::open(dir.path()).unwrap();
        assert!(stamp(&repo, &store).unwrap().head.contains("unborn"));
    }

    #[test]
    fn the_stamp_sees_writes_into_a_child_repository() {
        let dir = code_repo();
        let child = tempfile::tempdir().unwrap();
        git(child.path(), &["init", "-q", "--bare"]);
        git(child.path(), &["config", "user.name", "Test"]);
        git(
            child.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        let repo = Repo::open(dir.path()).unwrap();
        let store = ChildRepoStore::open(child.path()).unwrap();

        let before = stamp(&repo, &store).unwrap();
        store.put_index_bytes(b"{}").unwrap();
        assert_ne!(stamp(&repo, &store).unwrap(), before);
    }

    /// Die Zusage aus der Doku von [`Live`]: Dieselben geöffneten Handles
    /// sehen, was nach dem Öffnen geschrieben wurde — Neuladen muss nichts
    /// neu öffnen.
    #[test]
    fn the_same_open_handles_see_what_was_written_after_opening() {
        let dir = code_repo();
        let ctx = Context {
            repo: Repo::open(dir.path()).unwrap(),
            root: dir.path().to_path_buf(),
            store: Box::new(InRepoStore::open(dir.path()).unwrap()),
        };
        let reviews = ReviewStore::new(Repo::open(dir.path()).unwrap());
        let live = Live {
            ctx: &ctx,
            reviews: &reviews,
            name: "probe",
        };
        let before = live.stamp();
        assert!(live.load().unwrap().cards().is_empty());

        // Ein zweites Handle schreibt — wie der post-commit-Hook in einem
        // anderen Prozess.
        let writer = InRepoStore::open(dir.path()).unwrap();
        let session = Session::new(
            Agent {
                name: "claude-code".into(),
                version: "1".into(),
            },
            Model {
                provider: "anthropic".into(),
                id: "opus".into(),
            },
            Intent {
                request: "Live-Probe".into(),
                ..Intent::default()
            },
        );
        let redacted = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        writer.put(&redacted).unwrap();

        assert_ne!(live.stamp(), before);
        let cards = live.load().unwrap().cards();
        assert_eq!(cards.len(), 1);
    }

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
