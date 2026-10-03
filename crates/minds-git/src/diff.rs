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

use crate::oid::{BlobId, CommitId, TreeId};
use crate::{GitError, Repo, Result};

/// Pure line diff: zero-based ranges added in `after`, including replacements.
/// Uses Myers with Git's indentation heuristics, as does [`Repo::diff_commit`].
/// Line terminators are significant, including a missing final newline. Callers
/// processing untrusted blobs should bound their size before calling this.
pub fn added_line_ranges(before: &[u8], after: &[u8]) -> Vec<std::ops::Range<u32>> {
    use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
    let input = InternedInput::new(before, after);
    diff_with_slider_heuristics(Algorithm::Myers, &input)
        .hunks()
        .filter(|hunk| !hunk.after.is_empty())
        .map(|hunk| hunk.after)
        .collect()
}

/// Pure line diff: whether some hunk removes more lines from `before` than
/// it adds to `after` — content that vanished rather than being replaced in
/// place. Same algorithm as [`added_line_ranges`].
pub fn removes_lines(before: &[u8], after: &[u8]) -> bool {
    use gix::diff::blob::{Algorithm, InternedInput, diff_with_slider_heuristics};
    let input = InternedInput::new(before, after);
    diff_with_slider_heuristics(Algorithm::Myers, &input)
        .hunks()
        .any(|hunk| hunk.before.len() > hunk.after.len())
}

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
            .arg("--diff-algorithm=myers")
            .arg("--indent-heuristic")
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

/// Eine geänderte Datei eines Commits gegen seinen ersten Elternteil — als
/// Blob-Ids, nicht als Bytes: Der Aufrufer liest sie einzeln
/// ([`Repo::read_blob_id`]) und gibt sie wieder frei, damit ein Commit mit
/// vielen oder großen Dateien nie als Ganzes im Speicher liegt.
///
/// Umbenennungen erscheinen bewusst als Löschung plus Hinzufügung
/// (`--no-renames`): Die Reconciliation bewertet beide Pfade getrennt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedEntry {
    /// Repo-relativer Pfad — eine Identität, kein Anzeigetext. Nicht als
    /// UTF-8 lesbare Bytes sind ersetzt (`U+FFFD`), siehe `path_is_utf8`.
    pub path: String,
    /// `false`, wenn der Git-Pfad kein gültiges UTF-8 war: Dann ist `path`
    /// nur eine Anzeigeform, die mit anderen Pfaden kollidieren kann, und
    /// darf nicht als Identität verglichen werden.
    pub path_is_utf8: bool,
    /// Blob im Elternteil; `None` bei einer Hinzufügung.
    pub base: Option<BlobId>,
    /// Blob im Commit; `None` bei einer Löschung.
    pub committed: Option<BlobId>,
    /// Eine Seite ist ein Gitlink (Submodul-Zeiger): Diese Änderung hat
    /// keinen Blob, ist aber eine Änderung.
    pub gitlink: bool,
    /// Der Modus wechselt (etwa `100644` → `100755`, Datei → Symlink) oder
    /// eine neue Datei ist ausführbar.
    pub mode_changed: bool,
    /// Eine Seite ist ein Symlink: Sein Blob ist das Linkziel, kein Inhalt,
    /// den ein Schreib-Claim belegen könnte.
    pub symlink: bool,
}

/// Alle geänderten Dateien eines Commits gegen den ersten Elternteil.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitChanges {
    /// Der betrachtete Commit.
    pub commit: CommitId,
    /// Der erste Elternteil; `None` beim Wurzel-Commit.
    pub base: Option<CommitId>,
    /// Die geänderten Dateien, nach Pfad sortiert.
    pub files: Vec<ChangedEntry>,
}

impl Repo {
    /// Der erste Elternteil eines Commits — `None` beim Wurzel-Commit.
    ///
    /// Die Id stammt aus dem Commit-Objekt selbst; ob der Elternteil in
    /// diesem Klon **vorhanden** ist (Shallow Clone), sagt erst
    /// [`Repo::has_commit`].
    pub fn first_parent(&self, commit: CommitId) -> Result<Option<CommitId>> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;
        verify_object(commit.to_gix(), gix::objs::Kind::Commit, &object.data)?;
        Ok(object
            .parent_ids()
            .next()
            .map(|id| CommitId::from_gix(id.detach())))
    }

    /// Ob das Commit-Objekt in diesem Repository liegt — `false` etwa für
    /// den abgeschnittenen Elternteil eines Shallow Clones.
    pub fn has_commit(&self, commit: CommitId) -> bool {
        self.gix().has_object(commit.to_gix())
    }

    /// Die geänderten Dateien eines Commits gegen seinen **ersten**
    /// Elternteil, als Blob-Ids.
    ///
    /// Anders als [`Repo::diff_commit`] auch für Merge-Commits definiert (der
    /// erste Elternteil ist die Basis) und mit `-z`, damit Pfade mit
    /// Sonderzeichen nicht gequotet ankommen. Submodule (Gitlinks) tragen
    /// keinen Blob (`base`/`committed` = `None`), bleiben aber als Änderung
    /// mit `gitlink` erhalten. Liest nur, schreibt nichts, lädt nie nach
    /// (`GIT_NO_LAZY_FETCH`): In einem Partial Clone mit fehlenden Bäumen
    /// scheitert der Aufruf, fehlende Blobs fallen erst beim Lesen auf.
    pub fn commit_changes(&self, commit: CommitId) -> Result<CommitChanges> {
        let base = self.first_parent(commit)?;
        let mut cmd = Command::new("git");
        // Nichts darf den Diff schönen oder ins Netz gehen: keine
        // Ersatzobjekte (`refs/replace`), keine per `.gitmodules`/Config
        // ausgeblendeten Submodule, kein Nachladen in Partial Clones.
        cmd.env("GIT_NO_LAZY_FETCH", "1")
            .args(["-c", "protocol.allow=never"])
            .arg("--no-replace-objects")
            .arg("--git-dir")
            .arg(self.git_dir())
            .args(["diff-tree", "-r", "-z", "--no-renames", "--raw"])
            .args([
                "--ignore-submodules=none",
                "--no-relative",
                "--no-commit-id",
            ]);
        match base {
            Some(parent) => cmd.arg(parent.to_string()),
            None => cmd.arg("--root"),
        };
        let output = cmd
            .arg(commit.to_string())
            .output()
            .map_err(|err| GitError::diff(commit, err))?;
        if !output.status.success() {
            return Err(GitError::diff(
                commit,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let mut files = parse_raw(&output.stdout)
            .map_err(|reason| GitError::diff(commit, reason))?
            .into_iter()
            .filter(|f| f.base.is_some() || f.committed.is_some() || f.gitlink)
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(CommitChanges {
            commit,
            base,
            files,
        })
    }

    /// Ob `path` im Baum `tree` als Datei steht (Blob, Symlink oder
    /// Gitlink) — ein Verzeichnis zählt nicht.
    pub fn path_in_tree(&self, tree: TreeId, path: &str) -> Result<bool> {
        crate::objects::validate_path(path)?;
        let object = self
            .gix()
            .find_tree(tree.to_gix())
            .map_err(|err| GitError::read_object(tree, err))?;
        verify_object(tree.to_gix(), gix::objs::Kind::Tree, &object.data)?;
        Ok(object
            .lookup_entry_by_path(path)
            .map_err(|err| GitError::read_object(tree, err))?
            .is_some_and(|entry| !entry.mode().is_tree()))
    }

    /// Ob das Repository ein Partial Clone ist — `extensions.partialClone`
    /// (ältere Git-Versionen) oder ein Remote mit `promisor = true`: Dort
    /// fehlen Objekte planmäßig, anderswo ist ein fehlendes Objekt ein
    /// Defekt.
    pub fn is_partial_clone(&self) -> bool {
        let config = self.gix().config_snapshot();
        if config.string("extensions.partialClone").is_some() {
            return true;
        }
        config
            .plumbing()
            .sections_by_name("remote")
            .into_iter()
            .flatten()
            .any(|section| {
                // Git-Boolean: auch ein nackter Schlüssel ohne Wert ist wahr.
                let Some(header) = section.header().subsection_name() else {
                    return false;
                };
                let key = format!("remote.{header}.promisor");
                config.boolean(key.as_str()).unwrap_or(false)
            })
    }

    /// Ob der Baum in diesem Repository liegt — `false` etwa in einem
    /// Treeless Partial Clone (`--filter=tree:0`).
    pub fn has_tree(&self, id: TreeId) -> bool {
        self.gix().has_object(id.to_gix())
    }

    /// Ob der Blob in diesem Repository liegt — `false` etwa in einem
    /// Partial Clone (`--filter=blob:none`), der ihn erst nachladen müsste.
    pub fn has_blob(&self, id: BlobId) -> bool {
        self.gix().has_object(id.to_gix())
    }

    /// Liest einen Blob über seine Id, vollständig.
    pub fn read_blob_id(&self, id: BlobId) -> Result<Vec<u8>> {
        let blob = self
            .gix()
            .find_blob(id.to_gix())
            .map_err(|err| GitError::read_object(id, err))?;
        let data = blob.detach().data;
        verify_object(id.to_gix(), gix::objs::Kind::Blob, &data)?;
        Ok(data)
    }
}

/// Die gelesenen Bytes müssen zu der Id hashen, unter der sie angefragt
/// wurden. Ersatzobjekte (`refs/replace`) — oder eine Objektdatenbank, die
/// lügt — fallen so laut auf, statt einen Abgleich zu schönen; auf die
/// Replace-Konfiguration der Git-Schicht allein ist kein Verlass.
pub(crate) fn verify_object(
    requested: gix::ObjectId,
    kind: gix::objs::Kind,
    data: &[u8],
) -> Result<()> {
    let actual = gix::objs::compute_hash(requested.kind(), kind, data)
        .map_err(|err| GitError::read_object(requested, err))?;
    if actual == requested {
        Ok(())
    } else {
        Err(GitError::read_object(
            requested,
            format!("object content hashes to {actual} (replaced object?)"),
        ))
    }
}

/// Zerlegt die Ausgabe von `git diff-tree --raw -z`.
///
/// Rein und ohne I/O. Ein Eintrag ist `:<modus> <modus> <sha> <sha>
/// <status>\0<pfad>\0`. Der Pfad wird direkt nach seinem Kopf verbraucht —
/// ein Pfad, der selbst mit `:` beginnt, kann nicht als Kopf missverstanden
/// werden. Ein unlesbarer Eintrag ist ein Fehler, kein stilles Auslassen:
/// Eine verschluckte Änderung schönte jede Zählung.
fn parse_raw(out: &[u8]) -> std::result::Result<Vec<ChangedEntry>, String> {
    const ABSENT: &str = "000000";
    const GITLINK: &str = "160000";
    const SYMLINK: &str = "120000";
    const EXECUTABLE: &str = "100755";
    let mut entries = Vec::new();
    let mut tokens = out.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let malformed = || {
            let shown: String = String::from_utf8_lossy(token).chars().take(80).collect();
            format!("unreadable diff-tree entry {shown:?}")
        };
        let header = token.strip_prefix(b":").ok_or_else(malformed)?;
        let path = tokens.next().ok_or_else(malformed)?;
        let header = String::from_utf8_lossy(header);
        let fields: Vec<&str> = header.split_whitespace().collect();
        let [old_mode, new_mode, old_sha, new_sha, _status] = fields[..] else {
            return Err(malformed());
        };
        let side = |mode: &str, sha: &str| -> std::result::Result<Option<BlobId>, String> {
            if mode == ABSENT || mode == GITLINK {
                return Ok(None);
            }
            gix::ObjectId::from_hex(sha.as_bytes())
                .map(|id| Some(BlobId::from_gix(id)))
                .map_err(|_| malformed())
        };
        let (path, path_is_utf8) = match std::str::from_utf8(path) {
            Ok(path) => (path.to_owned(), true),
            Err(_) => (String::from_utf8_lossy(path).into_owned(), false),
        };
        let gitlink = old_mode == GITLINK || new_mode == GITLINK;
        let symlink = old_mode == SYMLINK || new_mode == SYMLINK;
        entries.push(ChangedEntry {
            path,
            path_is_utf8,
            base: side(old_mode, old_sha)?,
            committed: side(new_mode, new_sha)?,
            gitlink,
            // Ein Symlink-Wechsel ist schon als `symlink` benannt.
            mode_changed: !gitlink
                && !symlink
                && ((old_mode != ABSENT && new_mode != ABSENT && old_mode != new_mode)
                    || (old_mode == ABSENT && new_mode == EXECUTABLE)),
            symlink,
        });
    }
    Ok(entries)
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
    /// Auch wenn die Replace-Konfiguration verdreht gelesen würde: Ein
    /// Ersatzobjekt kommt nie als das angefragte zurück.
    #[test]
    fn replaced_objects_never_pass_as_the_requested_one() {
        let fixture = TempRepo::init();
        fixture.write_file("a.txt", "human\n");
        let first = fixture.commit("human");
        let original = fixture.git(&["rev-parse", "HEAD:a.txt"]);
        let fake = fixture.write_raw_object("blob", b"agent\n");
        fixture.git(&["replace", original.trim(), fake.trim()]);
        fixture.write_file("b.txt", "b\n");
        let second = fixture.commit("second");
        let fake_parent = fixture.commit("fake");
        let (first_hex, fake_hex) = (first.to_string(), fake_parent.to_string());
        fixture.git(&["replace", &first_hex, &fake_hex]);
        for setting in ["true", "false"] {
            fixture.git(&["config", "core.useReplaceRefs", setting]);
            let repo = Repo::open(fixture.path()).unwrap();
            let blob =
                BlobId::from_gix(gix::ObjectId::from_hex(original.trim().as_bytes()).unwrap());
            match repo.read_blob_id(blob) {
                Ok(bytes) => assert_eq!(bytes, b"human\n", "{setting}"),
                Err(err) => assert!(format!("{err:?}").contains("replaced"), "{err:?}"),
            }
            match repo.first_parent(second) {
                Ok(parent) => assert_eq!(parent, Some(first), "{setting}"),
                Err(err) => assert!(format!("{err:?}").contains("replaced"), "{err:?}"),
            }
        }
    }

    #[test]
    fn partial_clones_are_recognised_by_their_extension() {
        let fixture = TempRepo::init();
        assert!(!Repo::open(fixture.path()).unwrap().is_partial_clone());
        fixture.git(&["config", "remote.origin.promisor", "true"]);
        assert!(Repo::open(fixture.path()).unwrap().is_partial_clone());
        fixture.git(&["config", "--unset", "remote.origin.promisor"]);
        assert!(!Repo::open(fixture.path()).unwrap().is_partial_clone());
        fixture.git(&["config", "extensions.partialClone", "origin"]);
        assert!(Repo::open(fixture.path()).unwrap().is_partial_clone());
    }

    #[test]
    fn removal_hunks_are_not_replacements() {
        assert!(removes_lines(b"a\nguard\nb\n", b"a\nb\nnote\n"));
        assert!(!removes_lines(b"a\nold\nb\n", b"a\nnew\nb\n"));
        assert!(!removes_lines(b"a\n", b"a\nmore\n"));
        assert!(removes_lines(b"a\nb\nc\n", b"a\nX\n"));
    }

    #[test]
    fn pure_line_diff_matches_commit_diff() {
        let cases = [
            ("", "new\nfile\n"),
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("a\nb\n", "insert\na\nb\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("end\n", "end"),
            ("a\r\nb\r\n", "a\nb\r\n"),
            ("same\nsame\nend\n", "same\nend\n"),
            ("a\n\nb\n\nc\n", "a\n\nb\n\nb\n\nc\n"),
            (
                "fn a() {\n    a();\n}\n",
                "fn a() {\n    b();\n    a();\n}\n",
            ),
            ("old\n", ""),
        ];
        let fixture = crate::fixture::TempRepo::init();
        for (index, (before, _)) in cases.iter().enumerate() {
            fixture.write_file(&format!("{index}.txt"), before);
        }
        fixture.commit("base");
        for (index, (_, after)) in cases.iter().enumerate() {
            fixture.write_file(&format!("{index}.txt"), after);
        }
        let commit = fixture.commit("changes");
        let repo = crate::Repo::open(fixture.path()).unwrap();
        let diff = repo.diff_commit(commit).unwrap();
        for (index, (before, after)) in cases.iter().enumerate() {
            let path = format!("{index}.txt");
            let file = diff.files.iter().find(|f| f.path == path).unwrap();
            let git: Vec<_> = file
                .lines
                .iter()
                .filter(|line| line.kind == super::DiffKind::Added)
                .map(|line| line.new.unwrap())
                .collect();
            let pure: Vec<_> = super::added_line_ranges(before.as_bytes(), after.as_bytes())
                .into_iter()
                .flatten()
                .map(|line| line + 1)
                .collect();
            assert_eq!(pure, git, "{path}");
        }
    }

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

    fn sides(repo: &Repo, entry: &ChangedEntry) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let read = |id: Option<BlobId>| id.map(|id| repo.read_blob_id(id).unwrap());
        (read(entry.base), read(entry.committed))
    }

    #[test]
    fn commit_changes_carries_both_sides_and_splits_renames() {
        let fixture = TempRepo::init();
        fixture.write_file("keep.txt", "a\nb\n");
        fixture.write_file("old name.txt", "moved\n");
        fixture.write_file("gone.txt", "bye\n");
        let root = fixture.commit("base");
        fixture.write_file("keep.txt", "a\nB\nc\n");
        fixture.git(&["mv", "old name.txt", "neu\u{e4}.txt"]);
        fixture.git(&["rm", "-q", "gone.txt"]);
        let second = fixture.commit("change");

        let repo = Repo::open(fixture.path()).unwrap();
        let changes = repo.commit_changes(second).unwrap();
        assert_eq!(changes.base, Some(root));
        let summary: Vec<_> = changes
            .files
            .iter()
            .map(|f| {
                assert!(f.path_is_utf8);
                let (base, committed) = sides(&repo, f);
                (f.path.clone(), base, committed)
            })
            .collect();
        let b = |s: &str| Some(s.as_bytes().to_vec());
        assert_eq!(
            summary,
            [
                ("gone.txt".into(), b("bye\n"), None),
                ("keep.txt".into(), b("a\nb\n"), b("a\nB\nc\n")),
                ("neu\u{e4}.txt".into(), None, b("moved\n")),
                ("old name.txt".into(), b("moved\n"), None),
            ]
        );

        let first = repo.commit_changes(root).unwrap();
        assert_eq!(first.base, None);
        assert_eq!(first.files.len(), 3);
        assert!(first.files.iter().all(|f| f.base.is_none()));
        assert!(repo.has_commit(root));
        let (tree, root_tree) = (repo.tree_of(second).unwrap(), repo.tree_of(root).unwrap());
        assert!(repo.path_in_tree(tree, "keep.txt").unwrap());
        assert!(repo.path_in_tree(root_tree, "gone.txt").unwrap());
        assert!(!repo.path_in_tree(tree, "gone.txt").unwrap());
        assert!(
            changes
                .files
                .iter()
                .all(|f| f.committed.is_none_or(|b| repo.has_blob(b)))
        );
    }

    #[test]
    fn commit_changes_of_a_merge_use_the_first_parent_and_empty_is_empty() {
        let fixture = TempRepo::init();
        fixture.write_file("a.txt", "a\n");
        fixture.commit("base");
        let main = fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        fixture.git(&["checkout", "-q", "-b", "side"]);
        fixture.write_file("side.txt", "side\n");
        fixture.commit("side");
        fixture.git(&["checkout", "-q", main.trim()]);
        fixture.write_file("main.txt", "main\n");
        let first = fixture.commit("main");
        fixture.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let merge = fixture.rev_parse("HEAD");

        let repo = Repo::open(fixture.path()).unwrap();
        let changes = repo.commit_changes(merge).unwrap();
        assert_eq!(changes.base, Some(first));
        // Gegen den ersten Elternteil kam genau die Seitenlinie hinzu.
        let paths: Vec<_> = changes.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["side.txt"]);

        let empty = fixture.commit("empty");
        assert!(repo.commit_changes(empty).unwrap().files.is_empty());
    }

    #[test]
    fn raw_parsing_ignores_gitlinks_colon_paths_and_marks_non_utf8() {
        let zero = "0".repeat(40);
        let sha = "a".repeat(40);
        let mut out = format!(
            ":000000 100644 {zero} {sha} A\0:odd\0:160000 160000 {sha} {sha} M\0sub\0\
             :100644 100644 {sha} {sha} M\0"
        )
        .into_bytes();
        out.extend_from_slice(b"a\xff\0");
        let entries = parse_raw(&out).unwrap();
        let blob = BlobId::from_gix(gix::ObjectId::from_hex(sha.as_bytes()).unwrap());
        assert_eq!(
            entries,
            [
                ChangedEntry {
                    path: ":odd".into(),
                    path_is_utf8: true,
                    base: None,
                    committed: Some(blob),
                    gitlink: false,
                    mode_changed: false,
                    symlink: false,
                },
                ChangedEntry {
                    path: "sub".into(),
                    path_is_utf8: true,
                    base: None,
                    committed: None,
                    gitlink: true,
                    mode_changed: false,
                    symlink: false,
                },
                ChangedEntry {
                    path: "a\u{fffd}".into(),
                    path_is_utf8: false,
                    base: Some(blob),
                    committed: Some(blob),
                    gitlink: false,
                    mode_changed: false,
                    symlink: false,
                },
            ]
        );
        let chmod = format!(":100644 100755 {sha} {sha} M\0x\0");
        assert!(parse_raw(chmod.as_bytes()).unwrap()[0].mode_changed);
        let new_exec = format!(":000000 100755 {zero} {sha} A\0x\0");
        assert!(parse_raw(new_exec.as_bytes()).unwrap()[0].mode_changed);
        let link = format!(":000000 120000 {zero} {sha} A\0x\0");
        let link = &parse_raw(link.as_bytes()).unwrap()[0];
        assert!(link.symlink && !link.mode_changed);
        for broken in [
            format!(":100644 100644 {sha} M\0x\0"),
            format!(":100644 100644 {sha} nothex M\0x\0"),
            format!(":100644 100644 {sha} {sha} M\0"),
            format!(":100644 100644 {sha} {sha}\0x\0"),
            "garbage\0".to_string(),
        ] {
            assert!(parse_raw(broken.as_bytes()).is_err(), "{broken:?}");
        }
    }
}
