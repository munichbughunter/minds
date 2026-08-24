//! Test-Fixtures: eine echte redigierte Session und ein echtes Repository.
//!
//! # Die Session kommt aus der Pipeline
//!
//! Bewusst **keine** handgebaute [`RedactedSession`] — den Typ gibt es nur aus
//! der Redaction, und genau das soll auch im Test gelten. Ein Fixture, das am
//! vorgesehenen Weg vorbeibaut, prüft am Ende eine Bauform, die es in Produktion
//! nicht gibt.
//!
//! # Das Repository kommt von `git`
//!
//! Dieselbe Begründung wie in `minds-git`: Würden die Fixtures mit gix gebaut
//! und mit gix gelesen, wären Schreib- und Lesefehler symmetrisch und blieben
//! unsichtbar. Erst ein von echtem `git` erzeugtes Repo prüft die Frage, auf die
//! es ankommt — und umgekehrt zeigt erst echtes `git` auf dem Ergebnis, ob das,
//! was Minds schreibt, gewöhnliches Git ist. Für `cargo test` muss `git` im PATH
//! liegen; ausgeliefert wird weiterhin ein Binary ohne `git`-Abhängigkeit.
//!
//! Jeder Aufruf läuft ohne globale und systemweite Konfiguration und mit fester
//! Identität und festem Datum, damit nicht die Umgebung des Entwicklers
//! mitentscheidet (ein gesetztes `commit.gpgsign` reicht sonst für rote Tests,
//! die mit dem Code nichts zu tun haben).
//!
//! # Bar und nicht bar
//!
//! [`TempRepo::init`] legt ein Repository mit Arbeitsverzeichnis an (das Repo
//! des Codes), [`TempRepo::init_bare`] eines ohne (das Kontext-Repo des
//! Child-Backends — dort arbeitet niemand). Beide bekommen eine **lokale**
//! Identität: `commit_tree_to_ref` besteht darauf, und die globale ist hier
//! bewusst abgeklemmt.
//!
//! # Doppelung, mit Ablaufdatum
//!
//! `minds-git` hat dasselbe `TempRepo`, nur reicher — es ist dort `pub(crate)`
//! und damit hier nicht erreichbar. Diese schlanke Zweitfassung ist der
//! Stopgap; spätestens wenn `minds-cli` (M6) und `minds-reader` (M7) sie zum
//! dritten und vierten Mal brauchen, gehört die Fixture in `minds-git` hinter
//! ein `testing`-Feature.

use std::path::Path;
use std::process::Command;

use minds_core::{Agent, Intent, Model, Role, Session, Turn};
use minds_redact::{RedactedSession, RedactionConfig};
use tempfile::TempDir;

/// Eine redigierte Session mit `request` als Anliegen und einem Nutzer-Zug.
///
/// Der Text geht durch die vollständige Default-Pipeline; bei harmlosem Text
/// bleiben die Zähler null und `redaction.applied` wird gesetzt.
pub(crate) fn redacted(request: &str) -> RedactedSession {
    let mut session = Session::new(
        Agent {
            name: "claude-code".into(),
            version: "1.0.0".into(),
        },
        Model {
            provider: "anthropic".into(),
            id: "claude-opus-4".into(),
        },
        Intent {
            request: request.into(),
            ..Intent::default()
        },
    );
    session.turns.push(Turn {
        role: Role::User,
        text: request.into(),
        tool_calls: Vec::new(),
        parent: None,
        at: None,
    });

    RedactionConfig::default()
        .pipeline()
        .expect("Default-Pipeline hat Detektoren")
        .redact_session(session)
        .expect("harmloser Text läuft sauber durch")
}

/// Ein frisch initialisiertes Repository in einem Temp-Verzeichnis, das beim
/// Drop wieder verschwindet.
pub(crate) struct TempRepo {
    dir: TempDir,
}

impl TempRepo {
    /// Legt ein leeres Repository mit dem Branch `main` an — noch ohne Commit.
    pub(crate) fn init() -> Self {
        let dir = tempfile::tempdir().expect("Temp-Verzeichnis anlegen");
        let repo = Self { dir };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        configure_identity(repo.path());
        repo
    }

    /// Legt ein leeres **bares** Repository an — der Normalfall für ein
    /// Kontext-Repo, in dem niemand arbeitet.
    pub(crate) fn init_bare() -> Self {
        let dir = tempfile::tempdir().expect("Temp-Verzeichnis anlegen");
        let repo = Self { dir };
        repo.git(&["init", "--quiet", "--bare", "--initial-branch=main"]);
        configure_identity(repo.path());
        repo
    }

    /// Das Arbeitsverzeichnis des Repositories.
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Legt eine Datei an (inklusive Zwischenverzeichnisse) und staged sie.
    pub(crate) fn write_file(&self, rel_path: &str, content: &str) {
        let path = self.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Verzeichnis anlegen");
        }
        std::fs::write(&path, content).expect("Datei schreiben");
        self.git(&["add", rel_path]);
    }

    /// Committet, was gestaged ist.
    pub(crate) fn commit(&self, message: &str) {
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
    }

    /// Der Objekt-Hash einer Revision als Text.
    pub(crate) fn hash(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_owned()
    }

    /// Wie viele Objekte in der Objektdatenbank liegen — erreichbar oder nicht.
    ///
    /// Der Weg, „das zweite `put` hat wirklich nichts geschrieben" zu prüfen,
    /// ohne es Git zu glauben: Ein neuer Blob oder Baum taucht hier auf, auch
    /// wenn am Ende kein Ref auf ihn zeigt.
    pub(crate) fn object_count(&self) -> usize {
        self.git(&["cat-file", "--batch-check", "--batch-all-objects"])
            .lines()
            .count()
    }

    /// Führt `git` im Repo aus und gibt stdout zurück; bricht ab, wenn der
    /// Aufruf fehlschlägt.
    pub(crate) fn git(&self, args: &[&str]) -> String {
        git_in(self.path(), args)
    }

    /// Wie [`git`](Self::git), aber mit Eingabe auf stdin — für Plumbing wie
    /// `git mktree`.
    pub(crate) fn git_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("git")
            .arg("-C")
            .arg(self.path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("git starten");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("stdin schreiben");
        let out = child.wait_with_output().expect("git abwarten");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("git-Ausgabe ist UTF-8")
    }
}

/// Legt ein bares Repository an einem festen Pfad an — für Fälle, in denen der
/// Ort zum Test gehört (etwa ein Kontext-Repo, das über einen relativen Pfad
/// gefunden werden soll).
pub(crate) fn init_bare_at(path: &Path) {
    std::fs::create_dir_all(path).expect("Verzeichnis anlegen");
    git_in(
        path,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    configure_identity(path);
}

/// Schreibt Name und E-Mail in die **lokale** Config des Repositories.
///
/// Nicht nur in die Umgebung: gix liest die Identität aus der Konfiguration des
/// Repositories, in das geschrieben wird, und `commit_tree_to_ref` besteht auf
/// einer. Die Env-Variablen in [`git_in`] sieht nur das `git`-Kind.
fn configure_identity(repo: &Path) {
    git_in(repo, &["config", "user.name", "Minds Test"]);
    git_in(repo, &["config", "user.email", "test@example.invalid"]);
}

/// Führt `git` in `dir` aus und gibt stdout zurück; bricht ab, wenn der Aufruf
/// fehlschlägt.
fn git_in(dir: &Path, args: &[&str]) -> String {
    // Ein Pfad, den es nicht gibt: Git behandelt ihn wie eine leere
    // Konfiguration — portabler als /dev/null.
    let no_config = dir.join("keine.gitconfig");

    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", &no_config)
        .env("GIT_CONFIG_SYSTEM", &no_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "Minds Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Minds Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00+00:00")
        .output()
        .expect("`git` muss für die Tests im PATH liegen");

    assert!(
        output.status.success(),
        "git {args:?} in {dir:?} fehlgeschlagen:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git-Ausgabe ist UTF-8")
}
