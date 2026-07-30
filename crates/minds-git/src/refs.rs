//! Der Kontext-Ref: auflösen, anlegen, fortschreiben.
//!
//! Hier wird das Geschriebene aus `objects.rs` haltbar. Bis ein Ref auf einen
//! Baum zeigt, sind Blobs und Trees unerreichbar und würden von `git gc`
//! eingesammelt; [`Repo::commit_tree_to_ref`] schließt diese Lücke: Commit
//! erzeugen, Ref bewegen, fertig.
//!
//! # Warum der Ref ein Orphan ist
//!
//! `refs/minds/context` hat eine **eigene, von der Code-Historie getrennte**
//! Kette: Der erste Commit hat keine Eltern, jeder weitere hat genau einen —
//! den vorherigen Kontext-Commit. Kein Produktions-Commit wird je zum Elter.
//! Drei Konsequenzen, alle beabsichtigt:
//!
//! - **Ein Klon nur des Kontext-Refs zieht keinen Quellcode mit.** Wer nur die
//!   Sessions braucht (der Reader, ein Auditor), fetcht `refs/minds/context`
//!   und bekommt JSON — keine Historie, keine Blobs des Projekts.
//! - **Der Kontext taucht in keinem `git log` auf.** Er hängt an keinem Branch.
//! - **`git branch` zeigt ihn nicht** — nur `refs/heads/*` gilt dort als
//!   Branch. Das ist Punkt 8 der Definition of Done: Wer Minds nicht nutzt,
//!   merkt nichts.
//!
//! # Compare-and-Swap statt „letzter gewinnt"
//!
//! Zwei `minds capture`-Läufe können sich überschneiden — ein `post-commit`-Hook
//! und ein Aufruf von Hand reichen dafür schon. Deshalb wird der Ref nie blind
//! überschrieben: Der zuvor gelesene Stand geht als erwarteter Wert mit in die
//! Ref-Transaktion (das übernimmt gix, siehe [`Repo::commit_tree_to_ref`]).
//! Hat sich der Ref zwischenzeitlich bewegt, schlägt der Schreibvorgang fehl
//! statt die fremde Session zu überschreiben — [`GitError::RefRaced`]. Der
//! Aufrufer liest neu und versucht es erneut; verloren geht nichts.
//!
//! # Minds schreibt nur unterhalb von `refs/minds/`
//!
//! [`Repo::commit_tree_to_ref`] weist jeden Ref außerhalb dieses Namensraums ab.
//! Das ist eine Leitplanke gegen die eine Klasse Fehler, die man nicht wieder
//! gutmacht: ein falsch durchgereichter Ref-Name, der `refs/heads/main`
//! verschiebt. Minds hat dort nichts zu suchen — und ein Crate, das nur seinen
//! eigenen Namensraum anfassen kann, kann den Branch des Nutzers nicht kaputt
//! machen.

use crate::error::{GitError, Result};
use crate::oid::{CommitId, TreeId};
use crate::repo::Repo;

/// Der Ref, unter dem der In-Repo-Store liegt.
///
/// Default, nicht Gesetz: `minds init` (M6) macht ihn konfigurierbar — das
/// Child-Repo-Backend nutzt denselben Namen in einem anderen Repository.
pub const DEFAULT_CONTEXT_REF: &str = "refs/minds/context";

/// Der Namensraum, unterhalb dessen Minds schreiben darf.
///
/// Lokales Staging (`refs/minds/local/*`, die „Shadow-Branches" der Vision)
/// liegt ebenfalls darunter und ist damit ohne weiteres Zutun erlaubt.
pub const MINDS_REF_NAMESPACE: &str = "refs/minds/";

/// Was [`Repo::commit_tree_to_ref`] am Ref bewirkt hat.
///
/// Der Unterschied ist für den Aufrufer selten handlungsrelevant, aber gut zu
/// protokollieren — und [`RefUpdate::Unchanged`] ist der Beleg dafür, dass ein
/// wiederholtes `put` derselben Session nichts kostet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefUpdate {
    /// Den Ref gab es noch nicht; es entstand ein Wurzel-Commit ohne Eltern.
    Created(CommitId),
    /// Der Ref existierte; es entstand ein Commit mit dem bisherigen als Elter.
    Advanced(CommitId),
    /// Der Ref zeigte bereits auf genau diesen Baum — nichts geschrieben.
    Unchanged(CommitId),
}

impl RefUpdate {
    /// Der Commit, auf den der Ref jetzt zeigt.
    pub fn commit(&self) -> CommitId {
        match self {
            RefUpdate::Created(commit)
            | RefUpdate::Advanced(commit)
            | RefUpdate::Unchanged(commit) => *commit,
        }
    }

    /// Ob dabei ein neuer Commit entstanden ist.
    pub fn wrote_commit(&self) -> bool {
        !matches!(self, RefUpdate::Unchanged(_))
    }
}

impl Repo {
    /// Der Commit, auf den `reference` zeigt — `None`, wenn es den Ref nicht
    /// gibt.
    ///
    /// Zeigt der Ref auf ein annotiertes Tag, wird bis zum Commit geschält.
    pub fn commit_at(&self, reference: &str) -> Result<Option<CommitId>> {
        let Some(mut found) = self
            .gix()
            .try_find_reference(reference)
            .map_err(|err| GitError::reference(reference, err))?
        else {
            return Ok(None);
        };

        let id = found
            .peel_to_id()
            .map_err(|err| GitError::reference(reference, err))?
            .detach();

        Ok(Some(CommitId::from_gix(id)))
    }

    /// Alle Refs unterhalb von `prefix`, mit dem Commit, auf den sie zeigen —
    /// nach Ref-Namen sortiert.
    ///
    /// Das ist die Lese-Ergänzung zu [`Repo::commit_at`]: Wer den ganzen
    /// Namensraum braucht (`minds sync` muss wissen, *was* es zu schicken gibt;
    /// der Audit-Export, welche Reviews es gibt), soll dafür nicht `git
    /// for-each-ref` aufrufen müssen.
    ///
    /// Refs, die sich nicht bis auf einen Commit schälen lassen (ein Tag auf
    /// einen Blob, ein kaputter Ref), werden **übersprungen** statt gemeldet.
    /// Der Namensraum ist eine Menge, kein Vertrag: Ein fremder Eintrag darin
    /// darf den Aufrufer nicht scheitern lassen.
    pub fn refs_under(&self, prefix: &str) -> Result<Vec<(String, CommitId)>> {
        let platform = self
            .gix()
            .references()
            .map_err(|err| GitError::reference(prefix, err))?;
        let iter = platform
            .prefixed(prefix)
            .map_err(|err| GitError::reference(prefix, err))?;

        let mut out = Vec::new();
        for reference in iter {
            let Ok(mut reference) = reference else {
                continue;
            };
            let name = reference.name().as_bstr().to_string();
            let Ok(id) = reference.peel_to_id() else {
                continue;
            };
            out.push((name, CommitId::from_gix(id.detach())));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Schreibt `tree` als neuen Stand von `reference` und gibt zurück, was
    /// dabei passiert ist.
    ///
    /// Der Ref muss unterhalb von [`MINDS_REF_NAMESPACE`] liegen. Existiert er
    /// noch nicht, entsteht ein **Wurzel-Commit ohne Eltern** (der Orphan);
    /// sonst ein Commit mit dem bisherigen Stand als einzigem Elter. Zeigt der
    /// Ref bereits auf `tree`, passiert nichts — kein Leer-Commit.
    ///
    /// Der Ref-Wechsel läuft als Compare-and-Swap gegen den zuvor gelesenen
    /// Stand; siehe Modul-Doku.
    ///
    /// # Fehler
    ///
    /// - [`GitError::ForbiddenRef`] — `reference` liegt außerhalb von
    ///   `refs/minds/`.
    /// - [`GitError::Identity`] — es ist keine Git-Identität konfiguriert.
    /// - [`GitError::RefRaced`] — der Ref hat sich zwischenzeitlich bewegt.
    pub fn commit_tree_to_ref(
        &self,
        reference: &str,
        tree: TreeId,
        message: &str,
    ) -> Result<RefUpdate> {
        validate_minds_ref(reference)?;
        let parent = self.commit_at(reference)?;
        self.commit_tree_onto(reference, tree, parent, message)
    }

    /// Wie [`Repo::commit_tree_to_ref`], aber mit explizit übergebenem
    /// Erwartungswert.
    ///
    /// Crate-intern: Öffentlich wäre der Parameter eine Einladung, den
    /// Compare-and-Swap zu umgehen. Für Tests ist er die einzige Möglichkeit,
    /// einen Wettlauf ohne echte Nebenläufigkeit herzustellen.
    fn commit_tree_onto(
        &self,
        reference: &str,
        tree: TreeId,
        parent: Option<CommitId>,
        message: &str,
    ) -> Result<RefUpdate> {
        // Schon auf dem Stand? Dann kein Commit. Das hält die Kontext-Historie
        // frei von Rauschen und macht ein wiederholtes `put` gratis.
        if let Some(parent) = parent {
            if self.tree_of(parent)? == tree {
                return Ok(RefUpdate::Unchanged(parent));
            }
        }

        self.require_identity()?;

        let outcome = self.gix().commit(
            reference,
            message,
            tree.to_gix(),
            parent.map(CommitId::to_gix),
        );

        match outcome {
            Ok(id) => {
                let commit = CommitId::from_gix(id.detach());
                Ok(match parent {
                    Some(_) => RefUpdate::Advanced(commit),
                    None => RefUpdate::Created(commit),
                })
            }
            Err(err) => {
                // War es ein Wettlauf? Nachsehen, statt in gix' Fehlervarianten
                // zu raten: Steht am Ref etwas anderes als das, worauf wir
                // aufgesetzt haben, hat jemand dazwischengefunkt.
                let current = self.commit_at(reference)?;
                if current == parent {
                    Err(GitError::commit(reference, err))
                } else {
                    Err(GitError::ref_raced(reference, parent, current))
                }
            }
        }
    }

    /// Stellt sicher, dass eine Git-Identität konfiguriert ist.
    ///
    /// gix erfindet bewusst keinen Default-Nutzer („failing to provide a user
    /// is fatal"), und das ist hier genau richtig: Ein Audit-Record mit
    /// ausgedachtem Autor wäre schlimmer als ein Abbruch. Wir prüfen vorab, um
    /// eine Meldung zu liefern, mit der man etwas anfangen kann.
    fn require_identity(&self) -> Result<()> {
        if self.gix().committer().is_none() || self.gix().author().is_none() {
            return Err(GitError::Identity);
        }
        Ok(())
    }
}

/// Weist alles ab, was nicht unterhalb von [`MINDS_REF_NAMESPACE`] liegt.
///
/// Die *Syntax* des Namens prüft gix beim Anlegen (`refs/minds/..` oder
/// Steuerzeichen fliegen dort auf). Hier geht es nur um den Namensraum — siehe
/// Modul-Doku.
fn validate_minds_ref(reference: &str) -> Result<()> {
    let rest = reference.strip_prefix(MINDS_REF_NAMESPACE).unwrap_or("");
    if rest.is_empty() {
        return Err(GitError::forbidden_ref(reference));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    /// Ein Repo mit Code auf `main` und einem geschriebenen, aber noch nicht
    /// verankerten Session-Baum.
    fn repo_with_pending_tree() -> (TempRepo, Repo, TreeId) {
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");

        let repo = Repo::open(fixture.path()).unwrap();
        let blob = repo.write_blob(b"{\"session\":\"aa\"}").unwrap();
        let tree = repo
            .write_tree(None, [("sessions/b3/aa.json", blob)])
            .unwrap();

        (fixture, repo, tree)
    }

    #[test]
    fn commit_at_is_none_before_anything_was_written() {
        let (_fixture, repo, _tree) = repo_with_pending_tree();
        assert_eq!(repo.commit_at(DEFAULT_CONTEXT_REF).unwrap(), None);
    }

    #[test]
    fn first_write_creates_the_ref() {
        let (fixture, repo, tree) = repo_with_pending_tree();

        let update = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: erste Session")
            .unwrap();

        assert!(matches!(update, RefUpdate::Created(_)));
        assert!(update.wrote_commit());
        // Gegenprobe mit echtem git: Der Ref existiert und zeigt dorthin.
        assert_eq!(
            fixture.hash(DEFAULT_CONTEXT_REF),
            update.commit().to_string()
        );
        assert_eq!(
            repo.commit_at(DEFAULT_CONTEXT_REF).unwrap(),
            Some(update.commit())
        );
    }

    #[test]
    fn first_commit_is_an_orphan() {
        // Der Kern der Sache: keine Eltern, also keine Verbindung zur
        // Code-Historie.
        let (fixture, repo, tree) = repo_with_pending_tree();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: erste Session")
            .unwrap();

        let parents = fixture.git(&["log", "--format=%P", DEFAULT_CONTEXT_REF]);
        assert_eq!(parents.trim(), "", "Wurzel-Commit hat keine Eltern");
    }

    #[test]
    fn context_history_never_touches_the_code_history() {
        let (fixture, repo, tree) = repo_with_pending_tree();
        let code = fixture.rev_parse("refs/heads/main");

        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let second = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree2, "minds: 2")
            .unwrap();

        // Eigener Revwalk über die Kontext-Kette: genau unsere zwei Commits,
        // der Code-Commit ist von dort aus nicht erreichbar.
        let reachable: Vec<_> = repo
            .revwalk(second.commit())
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(reachable, vec![second.commit(), first.commit()]);
        assert!(!reachable.contains(&code));
    }

    #[test]
    fn second_write_advances_the_ref() {
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let second = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree2, "minds: 2")
            .unwrap();

        assert!(matches!(second, RefUpdate::Advanced(_)));
        assert_ne!(second.commit(), first.commit());
    }

    #[test]
    fn writing_the_same_tree_again_changes_nothing() {
        // Das idempotente `put` aus M4, eine Ebene tiefer: kein Leer-Commit.
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();
        let again = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: nochmal")
            .unwrap();

        assert_eq!(again, RefUpdate::Unchanged(first.commit()));
        assert!(!again.wrote_commit());
    }

    #[test]
    fn the_written_session_can_be_read_back_through_the_ref() {
        // Der Weg, den `minds why` später geht: Ref → Baum → Blob.
        let (_fixture, repo, tree) = repo_with_pending_tree();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let content = repo
            .read_blob_at(DEFAULT_CONTEXT_REF, "sessions/b3/aa.json")
            .unwrap();
        assert_eq!(content.as_deref(), Some(&b"{\"session\":\"aa\"}"[..]));
        assert_eq!(
            repo.list_blobs_at(DEFAULT_CONTEXT_REF).unwrap(),
            vec!["sessions/b3/aa.json".to_owned()]
        );
    }

    #[test]
    fn the_ref_stays_invisible_to_normal_git_usage() {
        // Punkt 8 der Definition of Done.
        let (fixture, repo, tree) = repo_with_pending_tree();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let branches = fixture.git(&["branch", "--list"]);
        assert!(
            !branches.contains("minds"),
            "sichtbar in git branch: {branches}"
        );

        // Auffindbar ist er trotzdem — nur eben nicht im Weg.
        let refs = fixture.git(&["for-each-ref", "--format=%(refname)", MINDS_REF_NAMESPACE]);
        assert_eq!(refs.trim(), DEFAULT_CONTEXT_REF);
    }

    #[test]
    fn refs_outside_the_minds_namespace_are_refused() {
        let (fixture, repo, tree) = repo_with_pending_tree();
        let before = fixture.rev_parse("refs/heads/main");

        for forbidden in ["refs/heads/main", "refs/minds/", "HEAD", "refs/tags/v1"] {
            let result = repo.commit_tree_to_ref(forbidden, tree, "sollte nicht gehen");
            assert!(
                matches!(result, Err(GitError::ForbiddenRef { .. })),
                "{forbidden} hätte abgewiesen werden müssen"
            );
        }

        // Und main steht unverändert da.
        assert_eq!(fixture.rev_parse("refs/heads/main"), before);
    }

    #[test]
    fn a_ref_that_moved_underneath_us_is_reported_as_a_race() {
        // Zwei parallele `minds capture`-Läufe: Der zweite darf den ersten
        // nicht überschreiben.
        let (_fixture, repo, tree) = repo_with_pending_tree();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();

        // Stand von *vor* dem ersten Schreiben: Wir glauben, den Ref gäbe es
        // noch nicht.
        let err = repo
            .commit_tree_onto(DEFAULT_CONTEXT_REF, tree2, None, "minds: 2")
            .unwrap_err();
        assert!(matches!(err, GitError::RefRaced { .. }), "{err}");
    }

    #[test]
    fn a_stale_parent_is_reported_as_a_race() {
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree2, "minds: 2")
            .unwrap();

        // Jetzt mit dem inzwischen veralteten ersten Commit als Erwartungswert.
        let blob = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();
        let tree3 = repo
            .write_tree(Some(tree2), [("sessions/b3/cc.json", blob)])
            .unwrap();
        let err = repo
            .commit_tree_onto(DEFAULT_CONTEXT_REF, tree3, Some(first.commit()), "minds: 3")
            .unwrap_err();

        assert!(matches!(err, GitError::RefRaced { .. }), "{err}");
    }

    #[test]
    fn a_race_leaves_the_ref_untouched() {
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let _ = repo.commit_tree_onto(DEFAULT_CONTEXT_REF, tree2, None, "minds: 2");

        // Nichts verloren: Der erste Stand steht noch.
        assert_eq!(
            repo.commit_at(DEFAULT_CONTEXT_REF).unwrap(),
            Some(first.commit())
        );
    }

    #[test]
    fn local_staging_refs_are_allowed() {
        // `refs/minds/local/*` — die Shadow-Branches der Vision.
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let update = repo.commit_tree_to_ref("refs/minds/local/wip", tree, "minds: wip");
        assert!(update.is_ok(), "{:?}", update.err());
    }
}
