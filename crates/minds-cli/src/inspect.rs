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

use minds_core::Session;
use minds_git::{CommitId, Head, MINDS_REF_NAMESPACE, Repo};
use minds_reader::Inspection;
use minds_reader::assurance::Reason;
use minds_reader::scope::NoScope;
use minds_store::{ContextStore, ReviewStore};
use minds_tui::{CommitVerify, Options, SessionAssurance, Source, Stamp, Start, VerifyVerdict};

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
        exe: std::env::current_exe()
            .map_err(|_| "the minds binary cannot be resolved")
            .and_then(|exe| {
                let cwd = std::env::current_dir().map_err(|_| "no working directory")?;
                pin_exe(&exe, &ctx.root, &cwd)
            }),
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
    /// Dieses Binary, festgehalten — für das Urteil `minds verify <commit>`
    /// im Verify-Tab. `Err` (Tests, Binary im Checkout): kein Urteil, der Tab
    /// sagt „not checked" mit diesem Grund.
    exe: Result<PinnedExe, &'static str>,
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

    fn verify(&self, commit: CommitId) -> Result<CommitVerify, String> {
        verify_commit(self.ctx, commit, self.exe.as_ref().map_err(|why| *why))
            .map_err(|err| minds_reader::sanitize(&err.to_string()))
    }
}

/// Das signaturabhängige Urteil über `commit` — dieselben Bausteine wie
/// `minds verify` (`session_assurance`, gegen `~/.ssh/allowed_signers`) und
/// derselbe Bereichs-Check. Strikt lesend.
fn verify_commit(
    ctx: &Context,
    commit: CommitId,
    exe: Result<&PinnedExe, &str>,
) -> Fallible<CommitVerify> {
    use crate::verify_cmd::{ArtifactState, scope::ScopeState};

    let store = ctx.store.as_ref();
    let ids = crate::verify_cmd::sessions_of_commit(ctx, commit)?;
    // Wie `minds verify`: ssh-keygen für Gegenzeichnungen außerhalb des
    // Checkouts aufgelöst — kein vom Agenten abgelegtes `ssh-keygen` im PATH.
    let trust = crate::verify_cmd::witness_trust::WitnessTrust::new(store, None)
        .with_anchor_program(
            std::env::var("PATH")
                .ok()
                .and_then(|path| crate::replay_cmd::find_ssh_keygen(&path, &ctx.root)),
        );
    let hex = commit.to_string();
    let mut sessions = Vec::new();
    // Je Session: ohne Seal (wie `verify`: keine Evidence-Kette, kein Scope).
    let mut unsealed = Vec::new();
    for (id, source) in &ids {
        let report = crate::verify_cmd::session_assurance(
            store,
            &trust,
            None,
            &ctx.root,
            *id,
            Some(*source),
            Some(&hex),
        )?;
        unsealed.push(
            report
                .reasons
                .iter()
                .any(|r| matches!(r, Reason::Legacy | Reason::NoSeal)),
        );
        sessions.push(SessionAssurance {
            session: *id,
            level: report.overall,
            reason: crate::verify_cmd::assurance::first_reason(&report)
                .map(|r| minds_reader::sanitize(&r)),
            tampered: report.reasons.contains(&Reason::IntegrityBroken),
            intent: report.intent,
        });
    }

    // Der Bereich — Session für Session wie `verify` (`scope::assess`: eigener
    // Anker, Claims, bezeugte Beobachtungen, fail-closed); aus verändertem
    // Material kein Befund. Ein Fehler hier kostet nur den Scope.
    let scope = (|| -> Fallible<Result<Vec<String>, String>> {
        let loaded: Vec<Option<Session>> = ids
            .iter()
            .map(|(id, _)| store.get(*id).ok().flatten())
            .collect();
        let readable: Vec<&Session> = loaded.iter().flatten().collect();
        let artifact = match crate::verify_cmd::artifact::assess(
            ctx,
            commit,
            &readable,
            &[],
            false,
            String::new(),
        ) {
            Ok(Ok(artifact)) => ArtifactState::Assessed(artifact),
            Ok(Err(why)) => ArtifactState::Unavailable(why),
            Err(err) => ArtifactState::Failed(minds_reader::sanitize(&err.to_string())),
        };
        let trusted = |seal_id: &minds_core::ContentHash, text: &str| {
            matches!(
                trust.signature(seal_id, text),
                minds_reader::assurance::SealSignature::Witness { .. }
            )
        };
        let mut outside = std::collections::BTreeSet::new();
        for (((id, _), (assured, session)), unsealed) in
            ids.iter().zip(sessions.iter().zip(&loaded)).zip(&unsealed)
        {
            if assured.tampered {
                return Ok(Err(NoScope::IntegrityViolated.word().to_owned()));
            }
            if *unsealed {
                return Ok(Err(NoScope::NoEvidenceChain.word().to_owned()));
            }
            let observations = match session {
                Some(session) => {
                    minds_reader::observations::session_observations(store, *id, session, &trusted)
                        .1
                }
                None => minds_reader::observations::WindowObservations {
                    observations: Vec::new(),
                    complete: true,
                },
            };
            match crate::verify_cmd::scope::assess(
                store,
                &ctx.repo,
                &ctx.root,
                &assured.intent,
                session.as_ref(),
                &artifact,
                &observations,
            ) {
                ScopeState::Assessed(findings) => {
                    outside.extend(findings.into_iter().map(|f| f.path));
                }
                ScopeState::NotAssessed(why) => return Ok(Err(why.word().to_owned())),
            }
        }
        // Das Gate von `verify`: Ohne abgeglichenen Commit fehlen dessen
        // Pfade — dann ist der Bereich nicht beurteilt, nicht „im Bereich".
        match &artifact {
            ArtifactState::Assessed(_) => Ok(Ok(outside.into_iter().collect())),
            ArtifactState::NoCommit => Ok(Err("no linked commit".to_owned())),
            ArtifactState::Unavailable(why) => Ok(Err((*why).to_owned())),
            ArtifactState::Failed(_) => Ok(Err("error".to_owned())),
        }
    })();
    let (out_of_scope, out_of_scope_paths, scope_note) = match scope {
        Ok(Ok(paths)) => (
            Some(
                paths
                    .iter()
                    .map(|p| minds_reader::sanitize_path(p))
                    .collect(),
            ),
            paths,
            None,
        ),
        Ok(Err(why)) => (None, Vec::new(), Some(why)),
        Err(err) => (
            None,
            Vec::new(),
            Some(minds_reader::sanitize(&err.to_string())),
        ),
    };
    Ok(CommitVerify {
        verdict: verdict_of(commit, exe),
        sessions,
        out_of_scope,
        out_of_scope_paths,
        scope_note,
    })
}

/// Wie lange `minds verify` für den Verify-Tab höchstens laufen darf.
const VERIFY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// So viel stderr wird gelesen (die erste Zeile ist der Grund).
const STDERR_CAP: usize = 4096;

/// Dieses Binary, beim Start festgehalten: kanonischer Pfad und Identität
/// der Datei. Das Urteil des Verify-Tabs soll von **diesem** Programm
/// kommen — nicht von dem, was später an seinem Pfad liegt.
#[derive(Debug, Clone)]
struct PinnedExe {
    path: std::path::PathBuf,
    identity: Identity,
    /// Das Verzeichnis, aus dem `minds inspect` seinen Kontext öffnete —
    /// dort startet auch `minds verify` (in einem Worktree derselbe Store).
    cwd: std::path::PathBuf,
}

/// Hält `exe` fest — oder lehnt ab, wenn es im Checkout liegt (dieselbe
/// Regel wie für `ssh-keygen`: was der Agent schreiben kann, startet minds
/// nicht als Prüfer).
fn pin_exe(
    exe: &std::path::Path,
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<PinnedExe, &'static str> {
    let path = exe
        .canonicalize()
        .map_err(|_| "the minds binary cannot be resolved")?;
    // Ohne prüfbare Datei-Identität (nicht Unix) kein Urteil: Ein
    // ausgetauschtes Binary bliebe sonst unbemerkt (fail-closed).
    if !cfg!(unix) {
        return Err("the minds binary cannot be pinned on this platform — run minds verify");
    }
    let root_real = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if path.starts_with(&root_real) || path.starts_with(root) {
        return Err("the minds binary lies inside the checkout — run an installed minds");
    }
    let identity = file_identity(&path);
    if identity.is_none() {
        return Err("the minds binary cannot be pinned");
    }
    Ok(PinnedExe {
        identity,
        path,
        cwd: cwd.to_path_buf(),
    })
}

/// Die Identität einer Datei: Gerät, Inode, Größe, mtime und ctime, je
/// mit Nanosekunden. ctime setzt kein Nutzer zurück — ein Überschreiben an
/// Ort und Stelle mit gleicher Größe und zurückgedrehter mtime fällt auf.
type Identity = Option<(u64, u64, u64, i64, i64, i64, i64)>;

#[cfg(unix)]
fn file_identity(path: &std::path::Path) -> Identity {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    ))
}

#[cfg(not(unix))]
fn file_identity(_path: &std::path::Path) -> Identity {
    None
}

/// Der Exit-Code von `minds verify` als Urteil — sonst der Grund.
///
/// Exit 1 heißt nur dann TAMPERED, wenn stderr leer ist: Auch ein
/// Bedienfehler endet mit 1, schreibt aber dorthin. Signal, 4 und alles
/// andere sind kein Urteil.
fn verdict_from(code: Option<i32>, stderr: &str) -> Result<VerifyVerdict, String> {
    let reason = || {
        minds_reader::sanitize(
            stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("minds verify failed"),
        )
    };
    match code {
        Some(0) => Ok(VerifyVerdict::Verified),
        // Exit 1 heißt auch „Fehler" — TAMPERED nur ohne stderr. Schreibt
        // `verify` je Warnungen nach stderr, wird TAMPERED hier zu „nicht
        // verfügbar": sicher, aber das Urteil verschwindet. Dann hier anpassen.
        Some(1) if stderr.trim().is_empty() => Ok(VerifyVerdict::Tampered),
        Some(2) => Ok(VerifyVerdict::Incomplete),
        Some(3) => Ok(VerifyVerdict::NotVerifiable),
        _ => Err(reason()),
    }
}

/// Das Urteil, wie `minds verify` es fällt — von `minds verify` selbst,
/// über seinen Exit-Code: Das Urteil wird nicht nachgebaut, also kann es
/// nicht abweichen. Gestartet wird das beim Start festgehaltene Binary
/// (unter Linux sein laufendes Abbild), nur wenn es noch dasselbe ist;
/// stdout verworfen, stderr begrenzt gelesen, nach [`VERIFY_DEADLINE`]
/// abgebrochen.
fn verdict_of(commit: CommitId, exe: Result<&PinnedExe, &str>) -> Result<VerifyVerdict, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let exe = exe.map_err(str::to_owned)?;
    if file_identity(&exe.path) != exe.identity {
        return Err("the minds binary changed since inspect started".into());
    }
    // Unter Linux das laufende Abbild selbst — ein Austausch der Datei
    // zwischen Prüfen und Starten erreicht es nicht.
    // Nur, wenn das laufende Abbild auch das festgehaltene Binary ist (in
    // Tests ist es das Test-Binary).
    let own = std::path::Path::new("/proc/self/exe");
    let program = if cfg!(target_os = "linux") && own.canonicalize().ok() == Some(exe.path.clone())
    {
        own.to_path_buf()
    } else {
        exe.path.clone()
    };
    let mut child = Command::new(program)
        .arg("verify")
        .arg(commit.to_string())
        .current_dir(&exe.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| minds_reader::sanitize(&err.to_string()))?;
    // stderr in einem eigenen Faden lesen (begrenzt, der Rest wird
    // geleert) — sonst blockierte ein volles Rohr das Kind.
    let mut pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.by_ref().take(STDERR_CAP as u64).read_to_end(&mut kept);
            let _ = std::io::copy(pipe, &mut std::io::sink());
        }
        kept
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= VERIFY_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("minds verify timed out".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(minds_reader::sanitize(&err.to_string()));
            }
        }
    };
    // Kein Enkel erbt stderr (git und ssh-keygen lesen ihr eigenes): Das
    // Rohr schließt mit dem Kind, `join` endet.
    let stderr = reader.join().unwrap_or_default();
    verdict_from(status.code(), &String::from_utf8_lossy(&stderr))
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

    /// Der signaturabhängige Teil des Verify-Tabs gegen ein echtes Repo: Die
    /// Session am Trailer wird gefunden und bewertet; ohne gebundenen Intent
    /// sagt der Scope, warum er nicht geprüft wurde.
    #[test]
    fn verify_commit_assesses_the_trailer_sessions() {
        let dir = code_repo();
        let store = InRepoStore::open(dir.path()).unwrap();
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
                request: "Verify-Probe".into(),
                ..Intent::default()
            },
        );
        let redacted = minds_redact::RedactionConfig::default()
            .pipeline()
            .unwrap()
            .redact_session(session)
            .unwrap();
        let id = store.put(&redacted).unwrap().id();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(
            dir.path(),
            &[
                "commit",
                "-q",
                "-m",
                &format!("probe\n\nMinds-Session-Id: {id}"),
            ],
        );
        let ctx = Context {
            repo: Repo::open(dir.path()).unwrap(),
            root: dir.path().to_path_buf(),
            store: Box::new(store),
        };
        let head = ctx.repo.head().unwrap().commit().unwrap();
        let verdict = super::verify_commit(&ctx, head, Err("not available")).unwrap();
        assert_eq!(verdict.sessions.len(), 1);
        assert_eq!(verdict.sessions[0].session, id);
        // Ohne Seal: nichts beobachtet, nur behauptet.
        assert_eq!(
            verdict.sessions[0].level,
            minds_reader::assurance::Assurance::A0Claimed
        );
        assert!(!verdict.sessions[0].tampered);
        // Wie `verify`: ohne Seal keine Evidence-Kette, kein Scope-Befund.
        assert_eq!(verdict.out_of_scope, None);
        assert_eq!(
            verdict.scope_note.as_deref(),
            Some(minds_reader::scope::NoScope::NoEvidenceChain.word())
        );
        // Ohne Binary kein Urteil — der Tab sagt dann „not checked".
        assert_eq!(verdict.verdict, Err("not available".to_string()));

        // Mit dem echten Binary: das Urteil ist der Exit-Code von
        // `minds verify` — eine Session ohne Seal ist NOT VERIFIABLE (3).
        let exe = std::env::current_exe()
            .ok()
            .and_then(|test| test.parent()?.parent().map(|dir| dir.join("minds")))
            .filter(|exe| exe.is_file());
        match exe {
            Some(exe) => {
                let pinned = super::pin_exe(&exe, &ctx.root, dir.path()).unwrap();
                let verdict = super::verify_commit(&ctx, head, Ok(&pinned)).unwrap();
                assert_eq!(verdict.verdict, Ok(minds_tui::VerifyVerdict::NotVerifiable));
            }
            None => {
                eprintln!("minds binary not built next to the test — exit-code path not exercised")
            }
        }
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
            exe: Err("not available"),
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

    /// Der Exit-Code von `minds verify` wird zum Urteil — und nur der.
    #[test]
    fn exit_codes_map_to_verdicts_and_nothing_else_is_a_verdict() {
        use minds_tui::VerifyVerdict::*;
        let v = super::verdict_from;
        assert_eq!(v(Some(0), ""), Ok(Verified));
        assert_eq!(v(Some(1), ""), Ok(Tampered));
        assert_eq!(v(Some(2), ""), Ok(Incomplete));
        assert_eq!(v(Some(3), ""), Ok(NotVerifiable));
        // Exit 1 mit stderr ist ein Bedienfehler, kein TAMPERED.
        assert_eq!(
            v(Some(1), "unknown command: verify\n"),
            Err("unknown command: verify".into())
        );
        assert_eq!(
            v(Some(4), "\nstore unreadable\nmore"),
            Err("store unreadable".into())
        );
        assert_eq!(v(None, ""), Err("minds verify failed".into()));
        assert_eq!(v(Some(101), ""), Err("minds verify failed".into()));
        // Der Grund wird entschärft.
        assert!(!v(Some(4), "\u{1b}[2Jbad").unwrap_err().contains('\u{1b}'));
    }

    /// Ein Binary im Checkout prüft nicht — und eine ausgetauschte Datei
    /// auch nicht.
    #[test]
    fn the_verify_binary_is_pinned_and_never_from_the_checkout() {
        let dir = code_repo();
        let inside = dir.path().join("bin-minds");
        std::fs::write(&inside, "#!/bin/sh\nexit 0\n").unwrap();
        assert!(super::pin_exe(&inside, dir.path(), dir.path()).is_err());

        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("minds");
        std::fs::write(&outside, "#!/bin/sh\nexit 0\n").unwrap();
        let pinned = super::pin_exe(&outside, dir.path(), dir.path()).unwrap();
        // An Ort und Stelle überschrieben, gleiche Größe, mtime zurückgedreht:
        // ctime verrät es.
        let reference = outside_dir.path().join("reference");
        std::fs::copy(&outside, &reference).unwrap();
        let ok = Command::new("touch")
            .arg("-r")
            .arg(&outside)
            .arg(&reference)
            .status()
            .unwrap();
        assert!(ok.success());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&outside, "#!/bin/sh\nexit 1\n").unwrap();
        let ok = Command::new("touch")
            .arg("-r")
            .arg(&reference)
            .arg(&outside)
            .status()
            .unwrap();
        assert!(ok.success());
        let head = Repo::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            super::verdict_of(head, Ok(&pinned)),
            Err("the minds binary changed since inspect started".into())
        );
        // Ausgetauscht (andere Größe): kein Urteil mehr.
        std::fs::write(&outside, "#!/bin/sh\n# swapped\nexit 0\n").unwrap();
        let head = Repo::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(
            super::verdict_of(head, Ok(&pinned)),
            Err("the minds binary changed since inspect started".into())
        );
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
