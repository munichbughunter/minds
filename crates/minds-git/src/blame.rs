//! Blame: von einer Zeile zum Commit — der erste Schritt in `minds why`.
//!
//! ```text
//! Buggy-Zeile → [hier] → Commit → Trailer → SessionId → Store → Prompt
//! ```
//!
//! `trailer.rs` deckt den zweiten Pfeil ab, dieses Modul den ersten. Damit ist
//! die Kette aus der Vision vollständig in `minds-git` abgebildet; alles
//! Weitere ist Store und Darstellung.
//!
//! # Warum ein Trait und nicht eine Funktion
//!
//! Weil hier zwei Motoren stehen, die dasselbe können sollen, und keiner von
//! beiden auf Dauer die richtige Antwort ist:
//!
//! - **[`GixBlame`]** läuft in-process. Das ist die Zielrichtung: ein statisch
//!   gelinktes Binary, das ohne `git` im PATH auskommt — Air-Gap,
//!   CI-Container, fremder Rechner.
//! - **[`ShellBlame`]** ruft `git blame --porcelain` auf. Das ist die
//!   Referenz-Implementierung schlechthin: Was `git` sagt, ist per Definition
//!   richtig, inklusive aller Sonderfälle, die gitoxide' junge Blame-Engine
//!   noch nicht kennt.
//!
//! Architektur-Prinzip 5 im Plan sagt genau das: gix für Reads, `git`-Shell als
//! Fallback hinter einem Trait. Der Trait ist die Naht, an der man den Motor
//! tauscht, ohne dass `minds-cli` oder `minds-reader` etwas davon merken.
//!
//! **Achtung, eine Zusage mit Sternchen:** Sobald [`ShellBlame`] läuft, braucht
//! Minds `git` im PATH. Das Binary bleibt statisch, die *Funktion* nicht mehr
//! autark. [`AutoBlame`] nimmt deshalb immer erst gix und greift zur Shell nur,
//! wenn gix nicht liefert.
//!
//! # Blame auf einem Commit, nicht auf dem Arbeitsverzeichnis
//!
//! Alle Methoden nehmen einen [`CommitId`] entgegen. `git blame` ohne Revision
//! nimmt den Arbeitsstand — dann verschöbe eine uncommittete Änderung die
//! Zeilennummern, und `minds why datei:42` zeigte je nach Editor-Zustand auf
//! eine andere Session. Für einen Audit-Record ist das nicht akzeptabel.
//! Nebeneffekt: Die Antwort „noch nicht committet" (Nullen-Hash) kann gar nicht
//! erst entstehen.
//!
//! Wessen Zeilennummern aus einem verschmutzten Arbeitsverzeichnis stammen, muss
//! die CLI wissen und warnen (M6) — diese Ebene ist deterministisch.
//!
//! # Wo die beiden Motoren auseinandergehen dürfen
//!
//! Bei umbenannten Dateien. `git blame` verfolgt die Datei über ihre
//! Umbenennung hinweg, [`GixBlame`] läuft mit gix' Vorgabe und damit **ohne**
//! Rename-Tracking (`Options::rewrites` ist `None`). Für eine Datei, die nie
//! umgezogen ist, sind beide deckungsgleich — das prüft `both_engines_agree`.
//! Sobald `minds why` über Umbenennungen hinweg richtig antworten muss, ist
//! `rewrites` die Stellschraube, und dann gehört ein Test dazu, der beide
//! Motoren an einem umbenannten Pfad vergleicht.
//!
//! # Was regulär vorkommt, ist kein Fehler
//!
//! Eine Datei, die es in diesem Commit nicht gibt, liefert eine leere Liste;
//! eine Zeile jenseits des Dateiendes (oder Zeile 0) liefert `None`. Dieselbe
//! Linie wie beim fehlenden Ref in `objects.rs`: Was der Aufrufer sonst wieder
//! herausfiltern müsste, kommt gar nicht erst als `Err` — und einer vergäße es.
//!
//! # Eine Zeile, ein Eintrag
//!
//! [`BlameProvider::blame_file`] gibt **pro Zeile** einen [`BlameLine`] zurück,
//! nicht die Bereiche, in denen beide Motoren intern rechnen. Der Reader aus M7
//! braucht genau diese Zuordnung (Zeile anklicken → Session), und ein Bereich
//! wäre dort in jeder Schleife wieder aufzulösen. Die Kosten sind ein paar
//! Bytes pro Zeile.

use std::cell::Cell;
use std::process::Command;

use crate::error::{GitError, Result};
use crate::oid::CommitId;
use crate::repo::Repo;

/// Die Zuordnung einer Zeile zu dem Commit, der sie zuletzt geschrieben hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlameLine {
    /// Zeilennummer in der Datei, **1-basiert** — so, wie `git blame` zählt und
    /// wie ein Editor sie anzeigt.
    pub line: u32,
    /// Der Commit, der diese Zeile zuletzt geändert hat.
    pub commit: CommitId,
}

/// Woher die Blame-Information kommt — gitoxide oder `git`.
///
/// Die Methoden nehmen den Commit, auf dem geblamed wird, ausdrücklich entgegen;
/// siehe Modul-Doku.
pub trait BlameProvider {
    /// Alle Zeilen von `path` im Zustand von `at`, aufsteigend nach
    /// Zeilennummer.
    ///
    /// Leer, wenn `path` in `at` keine Datei ist — kein Fehler.
    fn blame_file(&self, at: CommitId, path: &str) -> Result<Vec<BlameLine>>;

    /// Der Commit hinter *einer* Zeile; `None`, wenn es die Datei oder die
    /// Zeile dort nicht gibt.
    ///
    /// Die Vorgabe-Implementierung blamed die ganze Datei und sucht die Zeile
    /// heraus. Wer es billiger kann, überschreibt sie — [`ShellBlame`] tut das
    /// mit `-L`, weil dort jeder Aufruf einen Prozess kostet.
    fn blame_line(&self, at: CommitId, path: &str, line: u32) -> Result<Option<CommitId>> {
        Ok(self
            .blame_file(at, path)?
            .into_iter()
            .find(|entry| entry.line == line)
            .map(|entry| entry.commit))
    }
}

/// Blame über gitoxide — in-process, ohne `git` im PATH.
///
/// Braucht das `blame`-Feature an der gix-Abhängigkeit; das zieht `blob-diff`
/// nach, weil Blame die Zwischenstände diffen muss.
///
/// Rename-Tracking ist aus (gix' Vorgabe); wo das zählt, steht in der
/// Modul-Doku.
pub struct GixBlame<'repo> {
    repo: &'repo Repo,
}

impl<'repo> GixBlame<'repo> {
    /// Bindet die Implementierung an ein Repository.
    pub fn new(repo: &'repo Repo) -> Self {
        Self { repo }
    }
}

impl BlameProvider for GixBlame<'_> {
    fn blame_file(&self, at: CommitId, path: &str) -> Result<Vec<BlameLine>> {
        if blob_at(self.repo, at, path)?.is_none() {
            return Ok(Vec::new());
        }

        let repo = self.repo.gix();
        let mut resources = diff_cache(self.repo, path)?;

        // Kein Commit-Graph: Der ist eine Beschleunigung für große Historien
        // und muss erst geschrieben werden (`git commit-graph write`). Ihn hier
        // vorauszusetzen hieße, von einer Repo-Optimierung abzuhängen, die
        // niemand garantiert.
        let outcome = gix::blame::file(
            &repo.objects,
            at.to_gix(),
            None,
            &mut resources,
            path.into(),
            gix::blame::Options::default(),
        )
        .map_err(|err| GitError::blame(path, err))?;

        // gix rechnet in Bereichen und zählt Token ab 0; wir geben Zeilen aus
        // und zählen ab 1.
        let mut lines = Vec::new();
        for entry in outcome.entries {
            let commit = CommitId::from_gix(entry.commit_id);
            for offset in 0..entry.len.get() {
                lines.push(BlameLine {
                    line: entry.start_in_blamed_file + offset + 1,
                    commit,
                });
            }
        }

        lines.sort_unstable();
        Ok(lines)
    }
}

/// Blame über `git blame --porcelain` in einem Kindprozess.
///
/// # Warum `--porcelain`
///
/// Das Standardformat ist für Menschen: gekürzte Hashes, Spaltenausrichtung,
/// konfigurierbar über `blame.*`. `--porcelain` ist stabil, gibt volle Hashes
/// und stellt **jeder** Dateizeile eine Kopfzeile `<sha> <alt> <neu>` voran —
/// womit das Parsen auf „eine Kopfzeile, ein Eintrag" zusammenschrumpft.
/// Inhaltszeilen beginnen mit einem Tabulator und können deshalb nie mit einer
/// Kopfzeile verwechselt werden.
///
/// # Was diese Implementierung nicht tut
///
/// Sie setzt keine `blame.*`-Konfiguration außer Kraft. Ein gesetztes
/// `blame.ignoreRevsFile` verschiebt hier das Ergebnis und in [`GixBlame`]
/// nicht — wenn das je zum Problem wird, gehört die Entscheidung in die Config
/// von `minds init` und nicht hierher.
pub struct ShellBlame<'repo> {
    repo: &'repo Repo,
}

impl<'repo> ShellBlame<'repo> {
    /// Bindet die Implementierung an ein Repository.
    pub fn new(repo: &'repo Repo) -> Self {
        Self { repo }
    }

    /// Ruft `git blame` auf und gibt dessen stdout zurück.
    ///
    /// `--git-dir` statt eines Arbeitsverzeichnisses: Der Aufruf soll nicht
    /// davon abhängen, wo der Prozess gerade steht, und er soll auch in einem
    /// baren Repository funktionieren (Child-Repo-Backend, M4).
    fn run(&self, at: CommitId, path: &str, line: Option<u32>) -> Result<Vec<u8>> {
        let mut command = Command::new("git");
        command
            .arg("--git-dir")
            .arg(self.repo.git_dir())
            .arg("blame")
            .arg("--porcelain");

        if let Some(line) = line {
            command.arg("-L").arg(format!("{line},{line}"));
        }

        let output = command
            .arg(at.to_string())
            .arg("--")
            .arg(path)
            .output()
            .map_err(|err| GitError::blame(path, err))?;

        if !output.status.success() {
            return Err(GitError::blame(
                path,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }

        Ok(output.stdout)
    }
}

impl BlameProvider for ShellBlame<'_> {
    fn blame_file(&self, at: CommitId, path: &str) -> Result<Vec<BlameLine>> {
        if blob_at(self.repo, at, path)?.is_none() {
            return Ok(Vec::new());
        }
        parse_porcelain(&self.run(at, path, None)?)
    }

    fn blame_line(&self, at: CommitId, path: &str, line: u32) -> Result<Option<CommitId>> {
        // `-L` mit einer Zeile jenseits des Dateiendes ist für `git` ein
        // fataler Fehler. Da der Aufrufer eine Zeilennummer aus einem Editor
        // oder einem Stacktrace mitbringt, wird hier vorher nachgesehen: „gibt
        // es nicht" ist eine Antwort, kein Absturz.
        let Some(content) = blob_at(self.repo, at, path)? else {
            return Ok(None);
        };
        if line == 0 || line > line_count(&content) {
            return Ok(None);
        }

        Ok(parse_porcelain(&self.run(at, path, Some(line))?)?
            .first()
            .map(|entry| entry.commit))
    }
}

/// Nimmt gitoxide und weicht auf `git` aus, wenn das nicht trägt.
///
/// Die Reihenfolge ist die Aussage: in-process zuerst, weil das Ziel ein Binary
/// ohne `git`-Abhängigkeit ist; die Shell als Netz, damit eine junge
/// Blame-Engine niemand blockiert.
///
/// Scheitern beide, meldet der **Fallback** den Fehler. Der ist der
/// handhabbarere von beiden: „`git` nicht gefunden" oder eine Zeile aus dessen
/// stderr sagt einem Menschen, was zu tun ist; ein Fehler aus dem Innenleben
/// einer Blame-Engine selten.
///
/// Gibt gix einmal auf, merkt sich dieses Handle das und fragt es nicht wieder
/// — sonst zahlte jeder weitere Aufruf den vollen Blame-Durchlauf zweimal. Der
/// Merker hängt am Handle, nicht am Repository: Ein frisches [`Repo::blame`]
/// beginnt wieder bei gix.
pub struct AutoBlame<'repo> {
    repo: &'repo Repo,
    gix_failed: Cell<bool>,
}

impl<'repo> AutoBlame<'repo> {
    /// Bindet die Auswahl an ein Repository.
    pub fn new(repo: &'repo Repo) -> Self {
        Self {
            repo,
            gix_failed: Cell::new(false),
        }
    }

    /// Versucht es mit gitoxide, sofern das in diesem Handle noch nicht
    /// gescheitert ist. `None` heißt „nimm die Shell".
    fn try_gix<T>(&self, attempt: impl FnOnce(GixBlame<'_>) -> Result<T>) -> Option<T> {
        if self.gix_failed.get() {
            return None;
        }

        match attempt(GixBlame::new(self.repo)) {
            Ok(value) => Some(value),
            Err(_) => {
                self.gix_failed.set(true);
                None
            }
        }
    }
}

impl BlameProvider for AutoBlame<'_> {
    fn blame_file(&self, at: CommitId, path: &str) -> Result<Vec<BlameLine>> {
        match self.try_gix(|gix| gix.blame_file(at, path)) {
            Some(lines) => Ok(lines),
            None => ShellBlame::new(self.repo).blame_file(at, path),
        }
    }

    fn blame_line(&self, at: CommitId, path: &str, line: u32) -> Result<Option<CommitId>> {
        match self.try_gix(|gix| gix.blame_line(at, path, line)) {
            Some(commit) => Ok(commit),
            None => ShellBlame::new(self.repo).blame_line(at, path, line),
        }
    }
}

impl Repo {
    /// Blame für dieses Repository, mit automatischer Wahl des Motors.
    ///
    /// Wer die Wahl selbst treffen will (Tests, Vergleichsläufe), baut
    /// [`GixBlame`] oder [`ShellBlame`] direkt.
    pub fn blame(&self) -> AutoBlame<'_> {
        AutoBlame::new(self)
    }
}

/// Der Inhalt von `path` in `at` — `None`, wenn dort keine Datei steht.
///
/// Die gemeinsame Vorbedingung aller Implementierungen: Sie stellt sicher, dass
/// „Datei gibt es nicht" überall dieselbe Antwort ist und nicht einmal eine
/// leere Liste und einmal eine Fehlermeldung aus einem Kindprozess.
fn blob_at(repo: &Repo, at: CommitId, path: &str) -> Result<Option<Vec<u8>>> {
    let tree = repo.tree_of(at)?;
    repo.read_blob(tree, path)
}

/// Der Zwischenspeicher, den gix' Blame zum Diffen der Zwischenstände braucht.
fn diff_cache(repo: &Repo, path: &str) -> Result<gix::diff::blob::Platform> {
    repo.gix()
        .diff_resource_cache_for_tree_diff()
        .map_err(|err| GitError::blame(path, err))
}

/// Zählt Zeilen so, wie `git blame` sie nummeriert: Die letzte Zeile zählt auch
/// dann, wenn kein Zeilenumbruch mehr folgt.
fn line_count(content: &[u8]) -> u32 {
    if content.is_empty() {
        return 0;
    }
    let breaks = content.iter().filter(|byte| **byte == b'\n').count() as u32;
    if content.last() == Some(&b'\n') {
        breaks
    } else {
        breaks + 1
    }
}

/// Liest die Kopfzeilen aus `git blame --porcelain`.
///
/// Eine Kopfzeile ist `<sha> <Zeile-in-der-Quelle> <Zeile-in-der-Datei>`,
/// optional gefolgt von der Gruppengröße. Alles andere wird übergangen:
/// Inhaltszeilen beginnen mit einem Tabulator, die Zusatzangaben (`author`,
/// `summary`, `previous`, `boundary`, …) mit einem Schlüsselwort — beides kann
/// die Hex-Prüfung unten nicht bestehen.
fn parse_porcelain(output: &[u8]) -> Result<Vec<BlameLine>> {
    let mut lines = Vec::new();

    for raw in output.split(|byte| *byte == b'\n') {
        if raw.first() == Some(&b'\t') {
            continue;
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            continue;
        };

        let mut fields = text.split(' ');
        let (Some(sha), Some(_source_line), Some(file_line)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };

        if sha.len() < 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let (Ok(commit), Ok(line)) = (sha.parse::<CommitId>(), file_line.parse::<u32>()) else {
            continue;
        };

        lines.push(BlameLine { line, commit });
    }

    lines.sort_unstable();
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    /// Drei Zeilen aus zwei Commits: Zeile 2 stammt aus dem zweiten, der Rest
    /// aus dem ersten.
    fn repo_with_two_commits() -> (TempRepo, Repo, CommitId, CommitId) {
        let fixture = TempRepo::init();
        fixture.write_file("src/retry.rs", "eins\nzwei\ndrei\n");
        let first = fixture.commit("feat: drei Zeilen");
        fixture.write_file("src/retry.rs", "eins\nZWEI\ndrei\n");
        let second = fixture.commit("fix: zweite Zeile");

        let repo = Repo::open(fixture.path()).unwrap();
        (fixture, repo, first, second)
    }

    #[test]
    fn each_line_points_at_the_commit_that_wrote_it() {
        let (_fixture, repo, first, second) = repo_with_two_commits();

        let blame = repo.blame().blame_file(second, "src/retry.rs").unwrap();

        assert_eq!(
            blame,
            vec![
                BlameLine {
                    line: 1,
                    commit: first
                },
                BlameLine {
                    line: 2,
                    commit: second
                },
                BlameLine {
                    line: 3,
                    commit: first
                },
            ]
        );
    }

    #[test]
    fn both_engines_agree() {
        // Der Test, für den es den Trait gibt: Welcher Motor läuft, darf am
        // Ergebnis nichts ändern.
        let (_fixture, repo, _first, second) = repo_with_two_commits();

        let gix = GixBlame::new(&repo)
            .blame_file(second, "src/retry.rs")
            .unwrap();
        let shell = ShellBlame::new(&repo)
            .blame_file(second, "src/retry.rs")
            .unwrap();

        assert_eq!(gix, shell);
        assert_eq!(
            repo.blame().blame_file(second, "src/retry.rs").unwrap(),
            gix
        );
    }

    #[test]
    fn both_engines_agree_on_a_single_line() {
        let (_fixture, repo, first, second) = repo_with_two_commits();

        for line in 1..=3 {
            let gix = GixBlame::new(&repo)
                .blame_line(second, "src/retry.rs", line)
                .unwrap();
            let shell = ShellBlame::new(&repo)
                .blame_line(second, "src/retry.rs", line)
                .unwrap();
            assert_eq!(gix, shell, "Zeile {line}");
        }

        assert_eq!(
            repo.blame().blame_line(second, "src/retry.rs", 1).unwrap(),
            Some(first)
        );
    }

    #[test]
    fn blame_at_the_first_commit_knows_nothing_of_the_second() {
        let (_fixture, repo, first, _second) = repo_with_two_commits();

        let blame = repo.blame().blame_file(first, "src/retry.rs").unwrap();

        assert!(blame.iter().all(|entry| entry.commit == first), "{blame:?}");
    }

    #[test]
    fn blame_reads_the_commit_and_not_the_working_tree() {
        // Determinismus: Eine uncommittete Änderung darf die Antwort nicht
        // verschieben, sonst zeigte `minds why` je nach Arbeitsstand woanders
        // hin. Bewusst an `write_file` vorbei, damit nichts gestaged wird.
        let (fixture, repo, first, second) = repo_with_two_commits();
        std::fs::write(
            fixture.path().join("src/retry.rs"),
            "ganz\nanders\nund\nlaenger\n",
        )
        .unwrap();

        let blame = repo.blame().blame_file(second, "src/retry.rs").unwrap();

        assert_eq!(blame.len(), 3, "die Datei im Commit hat drei Zeilen");
        assert_eq!(blame[0].commit, first);
    }

    #[test]
    fn a_path_that_is_not_in_the_commit_yields_nothing() {
        let (_fixture, repo, _first, second) = repo_with_two_commits();
        let blame = repo.blame();

        assert!(
            blame
                .blame_file(second, "gibt/es/nicht.rs")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            blame.blame_line(second, "gibt/es/nicht.rs", 1).unwrap(),
            None
        );
    }

    #[test]
    fn a_line_outside_the_file_is_none() {
        // Beides Eingaben, die aus einem Editor oder Stacktrace kommen können.
        let (_fixture, repo, _first, second) = repo_with_two_commits();
        let blame = repo.blame();

        assert_eq!(blame.blame_line(second, "src/retry.rs", 0).unwrap(), None);
        assert_eq!(blame.blame_line(second, "src/retry.rs", 99).unwrap(), None);
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_line() {
        let fixture = TempRepo::init();
        fixture.write_file("ohne.txt", "eins\nzwei");
        let commit = fixture.commit("feat: ohne Zeilenumbruch am Ende");
        let repo = Repo::open(fixture.path()).unwrap();

        let blame = repo.blame().blame_file(commit, "ohne.txt").unwrap();

        assert_eq!(blame.len(), 2);
        assert_eq!(
            repo.blame().blame_line(commit, "ohne.txt", 2).unwrap(),
            Some(commit)
        );
    }

    #[test]
    fn all_engines_agree_that_a_missing_file_is_empty() {
        let (_fixture, repo, _first, second) = repo_with_two_commits();

        assert!(
            GixBlame::new(&repo)
                .blame_file(second, "weg.rs")
                .unwrap()
                .is_empty()
        );
        assert!(
            ShellBlame::new(&repo)
                .blame_file(second, "weg.rs")
                .unwrap()
                .is_empty()
        );
        assert!(
            repo.blame()
                .blame_file(second, "weg.rs")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn blame_leads_back_to_the_session_behind_a_line() {
        // Die ganze Kette in einem Test: Zeile → Commit → Trailer → SessionId.
        let fixture = TempRepo::init();
        let session: minds_core::SessionId = format!("b3-{}", "a".repeat(64)).parse().unwrap();
        let trailer = minds_core::Trailer::SessionId(session);

        fixture.write_file("src/retry.rs", "eins\nzwei\n");
        fixture.commit(&format!("feat: Retry\n\n{trailer}"));
        let repo = Repo::open(fixture.path()).unwrap();
        let head = repo.head().unwrap().commit().unwrap();

        let commit = repo
            .blame()
            .blame_line(head, "src/retry.rs", 2)
            .unwrap()
            .unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
    }

    #[test]
    fn line_count_counts_like_git() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"eins\n"), 1);
        assert_eq!(line_count(b"eins"), 1);
        assert_eq!(line_count(b"eins\nzwei\n"), 2);
        assert_eq!(line_count(b"eins\nzwei"), 2);
    }

    #[test]
    fn parse_porcelain_ignores_everything_but_the_headers() {
        // Handgebaute Ausgabe: Inhaltszeile mit Tabulator, Zusatzangaben, und
        // eine Zusammenfassung, die selbst wie Hex aussieht.
        let sha = "1e4f0b6a8c2d3e5f7a9b0c1d2e3f4a5b6c7d8e9f";
        let output = format!(
            "{sha} 1 1 2\n\
             author Minds Test\n\
             summary deadbeef 1 1\n\
             filename src/retry.rs\n\
             \teins\n\
             {sha} 2 2\n\
             \tzwei\n"
        );

        let parsed = parse_porcelain(output.as_bytes()).unwrap();

        let commit: CommitId = sha.parse().unwrap();
        assert_eq!(
            parsed,
            vec![BlameLine { line: 1, commit }, BlameLine { line: 2, commit },]
        );
    }
}
