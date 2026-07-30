//! Test-Fixtures: echte Repositories in einem Temp-Verzeichnis.
//!
//! # Warum die Fixtures `git` aufrufen und nicht gix
//!
//! Weil sonst gix seine eigenen Hausaufgaben korrigierte. Würden wir die Repos
//! mit gix bauen und mit gix lesen, wären Schreib- und Lesefehler symmetrisch
//! und blieben unsichtbar. Erst ein von **echtem `git`** erzeugtes Repo prüft
//! die Frage, auf die es ankommt: Liest Minds das, was Git tatsächlich
//! hinschreibt? (gitoxide selbst testet aus demselben Grund gegen
//! `git`-erzeugte Fixtures.)
//!
//! Das gilt nur für Tests: Ausgeliefert wird weiterhin ein Binary ohne
//! `git`-Abhängigkeit. Für `cargo test` muss `git` im PATH liegen.
//!
//! # Isolation
//!
//! Jeder Aufruf läuft ohne globale und systemweite Git-Konfiguration und mit
//! fester Identität und festem Datum. Sonst entschiede die Umgebung des
//! Entwicklers mit — ein gesetztes `commit.gpgsign`, ein `init.templateDir`
//! mit Hooks oder ein anderer `init.defaultBranch` machen Tests rot, die mit
//! dem Code nichts zu tun haben.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

use crate::oid::CommitId;

/// Ein frisch initialisiertes Repository in einem Temp-Verzeichnis, das beim
/// Drop wieder verschwindet.
pub(crate) struct TempRepo {
    dir: TempDir,
}

impl TempRepo {
    /// Legt ein leeres Repository mit dem Branch `main` an — noch ohne Commit,
    /// HEAD ist also ungeboren.
    pub(crate) fn init() -> Self {
        let dir = tempfile::tempdir().expect("Temp-Verzeichnis anlegen");
        let repo = Self { dir };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        // In die *lokale* Repo-Config, nicht nur in die Umgebung: gix liest die
        // Identität aus der Konfiguration, und `commit_tree_to_ref` besteht auf
        // einer. Die Env-Variablen unten sieht nur das `git`-Kind.
        repo.git(&["config", "user.name", "Minds Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo
    }

    /// Das Arbeitsverzeichnis des Repositories.
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Erzeugt einen leeren Commit mit dieser Message und gibt seine Id zurück.
    ///
    /// Leer, weil dieses Crate bislang nur die Historie liest, nicht deren
    /// Inhalt. Sobald M3 Blobs und Trees schreibt, kommen Fixtures mit echten
    /// Dateien dazu.
    pub(crate) fn commit(&self, message: &str) -> CommitId {
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.rev_parse("HEAD")
    }

    /// Legt eine Datei mit diesem Inhalt an (inklusive Zwischenverzeichnisse)
    /// und staged sie. Der nächste [`TempRepo::commit`] nimmt sie mit.
    pub(crate) fn write_file(&self, rel_path: &str, content: &str) {
        let path = self.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Verzeichnis anlegen");
        }
        std::fs::write(&path, content).expect("Datei schreiben");
        self.git(&["add", rel_path]);
    }

    /// Committet mit einer Message aus **rohen Bytes** — der einzige Weg, eine
    /// Message zu erzeugen, die kein gültiges UTF-8 ist (`-m` nimmt nur Strings).
    ///
    /// `--cleanup=verbatim`, damit Git die Bytes unangetastet lässt.
    pub(crate) fn commit_with_raw_message(&self, message: &[u8]) -> CommitId {
        // Innerhalb von `.git/`, damit die Datei nicht als unversionierte Datei
        // im Arbeitsverzeichnis auftaucht.
        let file = self.path().join(".git").join("MINDS_TEST_MSG");
        std::fs::write(&file, message).expect("Message-Datei schreiben");
        self.git(&[
            "commit",
            "--quiet",
            "--allow-empty",
            "--cleanup=verbatim",
            "-F",
            ".git/MINDS_TEST_MSG",
        ]);
        self.rev_parse("HEAD")
    }

    /// Schreibt ein Objekt aus rohen Bytes in die Objektdatenbank und gibt
    /// seinen Hash zurück.
    ///
    /// Der Weg zu Objekten, die `git commit` selbst nicht baut — etwa ein
    /// Commit mit `gpgsig`-Header, für den es sonst einen echten Schlüssel
    /// bräuchte. `--literally` nimmt die Bytes, wie sie kommen.
    pub(crate) fn write_raw_object(&self, kind: &str, object: &[u8]) -> String {
        let out = self.run_with_stdin(
            &["hash-object", "-t", kind, "-w", "--stdin", "--literally"],
            object,
        );
        String::from_utf8(out)
            .expect("git-Ausgabe ist UTF-8")
            .trim()
            .to_owned()
    }

    /// Löst eine Revision (`HEAD`, ein Branch-Name, …) zur [`CommitId`] auf.
    pub(crate) fn rev_parse(&self, rev: &str) -> CommitId {
        self.hash(rev)
            .parse()
            .expect("git rev-parse liefert einen vollen Hash")
    }

    /// Der Objekt-Hash einer Revision als Text — auch für Bäume und Blobs
    /// (`HEAD^{tree}`, `HEAD:pfad/datei.json`), für die es keinen eigenen
    /// Id-Typ mit `FromStr` gibt.
    pub(crate) fn hash(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_owned()
    }

    /// Wie [`TempRepo::git`], aber ohne UTF-8-Annahme — für Ausgaben, die rohe
    /// Objektbytes enthalten (`cat-file`).
    pub(crate) fn git_bytes(&self, args: &[&str]) -> Vec<u8> {
        self.run(args)
    }

    /// Führt `git` im Repo aus und gibt stdout zurück; bricht ab, wenn der
    /// Aufruf fehlschlägt.
    pub(crate) fn git(&self, args: &[&str]) -> String {
        String::from_utf8(self.run(args)).expect("git-Ausgabe ist UTF-8")
    }

    /// Führt `git` aus, ohne etwas auf stdin zu schicken.
    fn run(&self, args: &[&str]) -> Vec<u8> {
        let output = self
            .command(args)
            .output()
            .expect("`git` muss für die Tests im PATH liegen");
        Self::stdout_of(args, output)
    }

    /// Führt `git` aus und schiebt `stdin` hinein.
    fn run_with_stdin(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("`git` muss für die Tests im PATH liegen");

        // Die Pipe wird am Ende dieser Anweisung geschlossen — sonst wartete
        // `git` auf ein EOF, das nie käme.
        child
            .stdin
            .take()
            .expect("stdin ist verbunden")
            .write_all(stdin)
            .expect("stdin schreiben");

        let output = child.wait_with_output().expect("auf `git` warten");
        Self::stdout_of(args, output)
    }

    /// Der gemeinsame Kern: `git` in isolierter Umgebung.
    fn command(&self, args: &[&str]) -> Command {
        // Ein Pfad, den es nicht gibt: Git behandelt ihn wie eine leere
        // Konfiguration — portabler als /dev/null.
        let no_config = self.path().join("keine.gitconfig");

        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", &no_config)
            .env("GIT_CONFIG_SYSTEM", &no_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            // Falls ein Kommando doch einen Editor aufrufen will
            // (`commit --no-edit`, `rebase`): sofort erfolgreich beenden,
            // statt den Testlauf hängen zu lassen.
            .env("GIT_EDITOR", "true")
            .env("EDITOR", "true")
            .env("GIT_AUTHOR_NAME", "Minds Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Minds Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00+00:00");
        command
    }

    fn stdout_of(args: &[&str], output: std::process::Output) -> Vec<u8> {
        assert!(
            output.status.success(),
            "git {args:?} fehlgeschlagen:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}
