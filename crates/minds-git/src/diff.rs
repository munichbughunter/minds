//! Der Diff eines Commits gegen seinen Elternteil — die „Änderungen", die eine
//! Session hinterlassen hat, für den Reader Zeile für Zeile lesbar gemacht.
//!
//! ```text
//!   Commit ──diff-tree -p──►  je Datei: Hunks ──►  DiffLine{Kontext|Plus|Minus}
//! ```
//!
//! # Shell, wie der Blame-Fallback
//!
//! Der Diff läuft über den `git`-Prozess (`git diff-tree -p`), nicht über gix.
//! Das ist dieselbe Linie wie der Shell-Fallback beim Blame: Zum **Render-Zeitpunkt**
//! steht ein echtes Repository und damit `git` zur Verfügung. Die *Ausgabe* des
//! Readers bleibt davon unberührt selbsttragend — der Diff wird einmal in HTML
//! gegossen und braucht danach kein `git` mehr. Eine in-process-Variante über gix
//! kann später dazukommen, ohne dass sich diese API ändert.

use std::process::Command;

use crate::oid::CommitId;
use crate::{GitError, Repo, Result};

/// Wie eine Diff-Zeile zu lesen ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Unverändert, nur als Kontext gezeigt.
    Context,
    /// Hinzugefügt (`+`).
    Added,
    /// Entfernt (`-`).
    Removed,
    /// Ein Hunk-Kopf (`@@ … @@`) — der Sprung zur nächsten Änderung.
    Hunk,
}

/// Eine einzelne Zeile im Diff einer Datei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Art der Zeile.
    pub kind: DiffKind,
    /// Zeilennummer in der **alten** Fassung — bei Kontext und Entfernung.
    pub old: Option<u32>,
    /// Zeilennummer in der **neuen** Fassung — bei Kontext und Hinzufügung.
    pub new: Option<u32>,
    /// Der Text der Zeile, ohne führendes Diff-Zeichen und ohne Zeilenumbruch.
    pub text: String,
}

/// Der Diff einer einzelnen Datei innerhalb eines Commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    /// Pfad in der neuen Fassung; bei einer Löschung der alte Pfad.
    pub path: String,
    /// Wie viele Zeilen hinzukamen.
    pub added: usize,
    /// Wie viele Zeilen wegfielen.
    pub removed: usize,
    /// Binärdatei — keine Zeilen, nur die Tatsache der Änderung.
    pub binary: bool,
    /// Die Zeilen des Diffs, in Dateireihenfolge.
    pub lines: Vec<DiffLine>,
}

/// Alle Datei-Diffs eines Commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDiff {
    /// Der betrachtete Commit.
    pub commit: CommitId,
    /// Die geänderten Dateien.
    pub files: Vec<DiffFile>,
}

impl Repo {
    /// Der Diff eines Commits gegen seinen (ersten) Elternteil.
    ///
    /// Für den Wurzel-Commit (kein Elternteil) sorgt `--root` dafür, dass die
    /// ganze Einführung als Hinzufügung erscheint statt als leerer Diff.
    /// `--git-dir` statt Arbeitsverzeichnis, damit der Aufruf auch in einem
    /// baren Repository und unabhängig vom Prozess-Cwd trägt.
    pub fn diff_commit(&self, commit: CommitId) -> Result<CommitDiff> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(self.git_dir())
            .arg("diff-tree")
            .arg("--no-commit-id")
            .arg("--root") // Wurzel-Commit vollständig zeigen
            .arg("-p") // Patch-Format
            .arg("-r") // in Unterbäume absteigen
            .arg("--no-color")
            .arg("--unified=3")
            .arg(commit.to_string())
            .output()
            .map_err(|err| GitError::diff(commit, err))?;

        if !output.status.success() {
            return Err(GitError::diff(
                commit,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }

        Ok(CommitDiff {
            commit,
            files: parse_patch(&String::from_utf8_lossy(&output.stdout)),
        })
    }
}

/// Zerlegt die Ausgabe von `git diff-tree -p` in einen Diff pro Datei.
///
/// Rein und ohne I/O — der ganze Parser lässt sich damit gegen feste
/// Beispiel-Patches prüfen, ohne ein Repository zu bemühen.
fn parse_patch(patch: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut old_ln = 0u32;
    let mut new_ln = 0u32;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(DiffFile {
                path: path_from_header(rest),
                added: 0,
                removed: 0,
                binary: false,
                lines: Vec::new(),
            });
            continue;
        }

        let Some(file) = files.last_mut() else {
            continue; // Vorspann vor dem ersten „diff --git" — überspringen.
        };

        // Der `+++`-Kopf nennt den maßgeblichen Pfad am zuverlässigsten; bei
        // einer Löschung (`/dev/null`) bleibt der aus dem `diff --git`-Kopf.
        if let Some(new_path) = line.strip_prefix("+++ ") {
            if new_path != "/dev/null" {
                file.path = strip_ab(new_path);
            }
            continue;
        }
        if line.starts_with("--- ") {
            continue;
        }
        if line.starts_with("Binary files ") {
            file.binary = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            if let Some((o, n)) = hunk_starts(rest) {
                old_ln = o;
                new_ln = n;
            }
            file.lines.push(DiffLine {
                kind: DiffKind::Hunk,
                old: None,
                new: None,
                text: line.to_string(),
            });
            continue;
        }

        // Innerhalb eines Hunks: das erste Zeichen entscheidet.
        match line.as_bytes().first() {
            Some(b'+') => {
                file.added += 1;
                file.lines.push(DiffLine {
                    kind: DiffKind::Added,
                    old: None,
                    new: Some(new_ln),
                    text: line[1..].to_string(),
                });
                new_ln += 1;
            }
            Some(b'-') => {
                file.removed += 1;
                file.lines.push(DiffLine {
                    kind: DiffKind::Removed,
                    old: Some(old_ln),
                    new: None,
                    text: line[1..].to_string(),
                });
                old_ln += 1;
            }
            Some(b' ') => {
                file.lines.push(DiffLine {
                    kind: DiffKind::Context,
                    old: Some(old_ln),
                    new: Some(new_ln),
                    text: line[1..].to_string(),
                });
                old_ln += 1;
                new_ln += 1;
            }
            // `index …`, `new file mode …`, `\ No newline …`, Umbenennungs-Köpfe:
            // für die Anzeige ohne Belang.
            _ => {}
        }
    }

    files
}

/// Der Pfad aus einem `diff --git a/… b/…`-Kopf: die `b`-Seite, ersatzweise die
/// `a`-Seite. Für Pfade ohne Leerzeichen (der Normalfall) exakt; bei
/// Leerzeichen im Namen bleibt es eine brauchbare Näherung, die der `+++`-Kopf
/// gleich darauf korrigiert.
fn path_from_header(rest: &str) -> String {
    if let Some(pos) = rest.find(" b/") {
        return rest[pos + 3..].to_string();
    }
    strip_ab(rest.split_whitespace().next().unwrap_or(rest))
}

/// Streift ein führendes `a/` oder `b/` (den Diff-Präfix) vom Pfad.
fn strip_ab(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
}

/// Liest aus einem Hunk-Kopf `-<alt>,<n> +<neu>,<m> @@ …` die beiden
/// Startzeilen (alt, neu).
fn hunk_starts(rest: &str) -> Option<(u32, u32)> {
    let mut parts = rest.split_whitespace();
    let minus = parts.next()?.strip_prefix('-')?;
    let plus = parts.next()?.strip_prefix('+')?;
    let old = minus.split(',').next()?.parse().ok()?;
    let new = plus.split(',').next()?.parse().ok()?;
    Some((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_added_and_a_removed_line() {
        // Bewusst zeilenweise zusammengesetzt: eine `\`-Fortsetzung im
        // String-Literal fräse die führenden Leerzeichen weg — und genau die
        // markieren eine Kontextzeile im Diff.
        let patch = [
            "diff --git a/src/x.rs b/src/x.rs",
            "index 111..222 100644",
            "--- a/src/x.rs",
            "+++ b/src/x.rs",
            "@@ -1,2 +1,2 @@",
            "-alt",
            "+neu",
            " gleich",
        ]
        .join("\n");
        let files = parse_patch(&patch);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.path, "src/x.rs");
        assert_eq!(f.added, 1);
        assert_eq!(f.removed, 1);
        assert!(!f.binary);

        // Hunk-Kopf, dann minus, plus, Kontext.
        assert_eq!(f.lines[0].kind, DiffKind::Hunk);
        assert_eq!(f.lines[1].kind, DiffKind::Removed);
        assert_eq!(f.lines[1].text, "alt");
        assert_eq!(f.lines[1].old, Some(1));
        assert_eq!(f.lines[1].new, None);
        assert_eq!(f.lines[2].kind, DiffKind::Added);
        assert_eq!(f.lines[2].text, "neu");
        assert_eq!(f.lines[2].new, Some(1));
        assert_eq!(f.lines[3].kind, DiffKind::Context);
        assert_eq!(f.lines[3].old, Some(2));
        assert_eq!(f.lines[3].new, Some(2));
    }

    #[test]
    fn a_new_file_takes_its_path_from_the_plus_header() {
        let patch = "diff --git a/neu.txt b/neu.txt\n\
                     new file mode 100644\n\
                     index 000..abc\n\
                     --- /dev/null\n\
                     +++ b/neu.txt\n\
                     @@ -0,0 +1,2 @@\n\
                     +erste\n\
                     +zweite\n";
        let files = parse_patch(patch);
        assert_eq!(files[0].path, "neu.txt");
        assert_eq!(files[0].added, 2);
        assert_eq!(files[0].removed, 0);
        assert_eq!(files[0].lines[1].new, Some(1));
        assert_eq!(files[0].lines[2].new, Some(2));
    }

    #[test]
    fn a_deleted_file_keeps_its_path_from_the_git_header() {
        let patch = "diff --git a/weg.txt b/weg.txt\n\
                     deleted file mode 100644\n\
                     index abc..000\n\
                     --- a/weg.txt\n\
                     +++ /dev/null\n\
                     @@ -1,1 +0,0 @@\n\
                     -war da\n";
        let files = parse_patch(patch);
        assert_eq!(files[0].path, "weg.txt");
        assert_eq!(files[0].removed, 1);
        assert_eq!(files[0].added, 0);
    }

    #[test]
    fn a_binary_file_is_flagged_and_carries_no_lines() {
        let patch = "diff --git a/bild.png b/bild.png\n\
                     index abc..def 100644\n\
                     Binary files a/bild.png and b/bild.png differ\n";
        let files = parse_patch(patch);
        assert_eq!(files[0].path, "bild.png");
        assert!(files[0].binary);
        assert!(files[0].lines.is_empty());
    }

    #[test]
    fn several_files_are_split_apart() {
        let patch = "diff --git a/eins b/eins\n\
                     --- a/eins\n\
                     +++ b/eins\n\
                     @@ -1 +1 @@\n\
                     -a\n\
                     +b\n\
                     diff --git a/zwei b/zwei\n\
                     --- a/zwei\n\
                     +++ b/zwei\n\
                     @@ -1 +1 @@\n\
                     -c\n\
                     +d\n";
        let files = parse_patch(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "eins");
        assert_eq!(files[1].path, "zwei");
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(parse_patch("").is_empty());
    }

    // --- gegen ein echtes Repository ---------------------------------------

    use crate::fixture::TempRepo;

    #[test]
    fn diff_commit_reads_a_real_change() {
        let fixture = TempRepo::init();
        fixture.write_file("src/x.rs", "eins\nzwei\n");
        fixture.commit("feat: zwei Zeilen");
        fixture.write_file("src/x.rs", "eins\nZWEI\ndrei\n");
        let second = fixture.commit("fix: zweite Zeile, dritte dazu");

        let repo = Repo::open(fixture.path()).unwrap();
        let diff = repo.diff_commit(second).unwrap();

        assert_eq!(diff.commit, second);
        assert_eq!(diff.files.len(), 1);
        let f = &diff.files[0];
        assert_eq!(f.path, "src/x.rs");
        assert_eq!(f.added, 2); // ZWEI, drei
        assert_eq!(f.removed, 1); // zwei
    }

    #[test]
    fn diff_of_the_root_commit_is_a_full_addition() {
        let fixture = TempRepo::init();
        fixture.write_file("a.txt", "x\ny\n");
        let root = fixture.commit("erster Commit");

        let repo = Repo::open(fixture.path()).unwrap();
        let diff = repo.diff_commit(root).unwrap();

        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "a.txt");
        assert_eq!(diff.files[0].added, 2);
        assert_eq!(diff.files[0].removed, 0);
    }
}
