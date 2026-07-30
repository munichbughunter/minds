//! Gemeinsame Hilfen für die Integrationstests.
//!
//! # Warum das hier ein zweites Mal steht
//!
//! `src/fixture.rs` ist `#[cfg(test)]` und damit nur den Unit-Tests *im* Crate
//! zugänglich. Das ist kein Mangel, sondern der Sinn dieser Test-Ebene:
//! Integrationstests sehen genau das, was `minds-store`, `minds-cli` und
//! `minds-reader` später auch sehen — die öffentliche API und sonst nichts.
//! Kein `pub(crate)`, kein `gix()`, kein Zugriff auf Interna.
//!
//! Die Doppelung ist deshalb Absicht und keine fehlende Abstraktion: Würde
//! `TempRepo` geteilt, wäre die Grenze, um die es hier geht, wieder offen.
//! Diese Kopie ist bewusst kleiner — sie kann nur, was die Integrationstests
//! brauchen.

// Jedes Test-Binary übersetzt dieses Modul für sich und benutzt nur einen Teil
// davon; ungenutzte Helfer sind hier deshalb kein Signal.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use minds_core::SessionId;
use minds_git::{CommitId, DEFAULT_CONTEXT_REF, Repo};
use tempfile::TempDir;

/// Ein frisch initialisiertes Repository in einem Temp-Verzeichnis, das beim
/// Drop wieder verschwindet.
pub struct TempRepo {
    dir: TempDir,
}

impl TempRepo {
    /// Legt ein leeres Repository mit dem Branch `main` und fester Identität an.
    pub fn init() -> Self {
        let dir = tempfile::tempdir().expect("Temp-Verzeichnis anlegen");
        let repo = Self { dir };
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        // In die lokale Repo-Config: gix liest die Identität von dort, und
        // `commit_tree_to_ref` besteht auf einer.
        repo.git(&["config", "user.name", "Minds Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo
    }

    /// Das Arbeitsverzeichnis des Repositories.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Legt eine Datei an (inklusive Zwischenverzeichnisse) und staged sie.
    pub fn write_file(&self, rel_path: &str, content: &str) {
        let path = self.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Verzeichnis anlegen");
        }
        std::fs::write(&path, content).expect("Datei schreiben");
        self.git(&["add", rel_path]);
    }

    /// Committet das Gestagte und gibt die Id des neuen Commits zurück.
    pub fn commit(&self, message: &str) -> CommitId {
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.rev_parse("HEAD")
    }

    /// Löst eine Revision zur [`CommitId`] auf.
    pub fn rev_parse(&self, rev: &str) -> CommitId {
        self.hash(rev)
            .parse()
            .expect("git rev-parse liefert einen vollen Hash")
    }

    /// Der Objekt-Hash einer Revision als Text — auch für Bäume und Blobs
    /// (`HEAD^{tree}`, `HEAD:pfad/datei.rs`).
    pub fn hash(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev]).trim().to_owned()
    }

    /// Führt `git` aus und gibt stdout zurück; bricht ab, wenn der Aufruf
    /// fehlschlägt.
    pub fn git(&self, args: &[&str]) -> String {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "git {args:?} fehlgeschlagen:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git-Ausgabe ist UTF-8")
    }

    /// Ob `git` mit Erfolg beendet — für Fragen, deren Antwort „nein" ist
    /// (`cat-file -e` auf ein Objekt, das nicht da sein soll).
    pub fn git_ok(&self, args: &[&str]) -> bool {
        self.command(args).status.success()
    }

    /// `git` in isolierter Umgebung: ohne globale Konfiguration, mit fester
    /// Identität und festem Datum. Sonst entschiede die Umgebung des
    /// Entwicklers mit — ein gesetztes `commit.gpgsign` oder ein
    /// `init.templateDir` mit Hooks machen Tests rot, die mit dem Code nichts
    /// zu tun haben.
    fn command(&self, args: &[&str]) -> std::process::Output {
        // Ein Pfad, den es nicht gibt: Git behandelt ihn wie eine leere
        // Konfiguration — portabler als /dev/null.
        let no_config = self.path().join("keine.gitconfig");

        Command::new("git")
            .args(args)
            .current_dir(self.path())
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
            .expect("`git` muss für die Tests im PATH liegen")
    }
}

/// Eine gültige [`SessionId`] aus einem wiederholten Hex-Zeichen.
///
/// Echte Ids entstehen als `blake3(canonical_json(session))` in `minds-core`;
/// für diese Ebene ist die Id nur ein Wert, der unverändert durch Trailer,
/// Store und zurück laufen muss.
pub fn session_id(hex: char) -> SessionId {
    format!("b3-{}", hex.to_string().repeat(64))
        .parse()
        .expect("gültige SessionId")
}

/// Der Pfad, unter dem eine Session im Kontext-Baum liegt.
///
/// Das Layout gehört `minds-store` (M4) — hier steht es, damit der Rückweg
/// „SessionId → Blob" überhaupt geprüft werden kann.
pub fn session_path(session: SessionId) -> String {
    let hex = session
        .to_string()
        .strip_prefix("b3-")
        .expect("SessionId beginnt mit b3-")
        .to_owned();
    format!("sessions/b3/{hex}.json")
}

/// Legt `body` als Session unter [`DEFAULT_CONTEXT_REF`] ab und gibt ihren Pfad
/// zurück.
///
/// Das ist in Kurzform, was `minds capture` später tut: Blob schreiben, in den
/// bestehenden Baum einhängen, Ref fortschreiben.
pub fn store_session(repo: &Repo, session: SessionId, body: &str) -> String {
    let path = session_path(session);
    let blob = repo.write_blob(body.as_bytes()).expect("Blob schreiben");
    let base = repo
        .tree_at(DEFAULT_CONTEXT_REF)
        .expect("Kontext-Baum lesen");
    let tree = repo
        .write_tree(base, [(path.as_str(), blob)])
        .expect("Baum schreiben");
    repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: Session abgelegt")
        .expect("Kontext-Ref fortschreiben");
    path
}
