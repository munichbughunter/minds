//! Revwalk: die erreichbare Historie ablaufen ([`Repo::revwalk`]).
//!
//! Der Walk ist die Grundlage von `minds fsck` („ist jeder Trailer im Repo
//! auflösbar?") und später von der Attribution. Er liefert **jeden von `tip`
//! aus erreichbaren Commit genau einmal**.
//!
//! # Die Zusage ist Erreichbarkeit, nicht Reihenfolge
//!
//! Der Walk läuft in Breitensuche vom Tip weg. Bei linearer Historie heißt das
//! schlicht „neuester zuerst"; sobald Merges im Spiel sind, ist die Reihenfolge
//! zwischen den Zweigen nicht definiert. Wer eine bestimmte Ordnung braucht,
//! sortiert selbst — die Menge stimmt in jedem Fall.
//!
//! Bewusst **nicht** nach Commit-Zeit sortiert: Zeitstempel in einem Git-Graph
//! lügen regelmäßig (Rebase schreibt sie um, Cherry-Pick trägt alte Daten
//! weiter, verstellte Uhren erzeugen Commits „vor" ihren Eltern). Die Kanten
//! des Graphen lügen nicht. Für einen Audit-Record ist das der Unterschied
//! zwischen „belastbar" und „meistens richtig".
//!
//! # Ein unbekannter Startpunkt ist ein Fehler, keine leere Historie
//!
//! [`Repo::revwalk`] löst `tip` auf, **bevor** es losläuft. Der Grund ist
//! fail-closed: `minds fsck` würde eine leere Historie als „nichts zu
//! beanstanden" lesen. Ein fehlendes Objekt muss laut sein — es bedeutet einen
//! kaputten oder unvollständig gefetchten Klon, und genau das soll der Nutzer
//! erfahren.

use crate::error::{GitError, Result};
use crate::oid::CommitId;
use crate::repo::Repo;

impl Repo {
    /// Läuft die von `tip` aus erreichbare Historie ab, Commit für Commit.
    ///
    /// Jeder erreichbare Commit kommt genau einmal; zur Reihenfolge siehe
    /// Modul-Doku. Der Iterator liefert `Result`, weil Objekte auch mitten im
    /// Lauf fehlen können (unvollständiger Klon, beschädigtes Objekt) — der
    /// Aufrufer entscheidet, ob er abbricht (`collect::<Result<Vec<_>>>()`)
    /// oder Defekte sammelt.
    ///
    /// # Fehler
    ///
    /// [`GitError::Revwalk`], wenn `tip` nicht im Repository liegt oder der
    /// Lauf nicht starten kann.
    pub fn revwalk(&self, tip: CommitId) -> Result<impl Iterator<Item = Result<CommitId>> + '_> {
        let gix = self.gix();

        // Startpunkt zuerst auflösen — siehe Modul-Doku (fail-closed).
        gix.find_commit(tip.to_gix())
            .map_err(|err| GitError::revwalk(tip, err))?;

        let walk = gix
            .rev_walk(Some(tip.to_gix()))
            .all()
            .map_err(|err| GitError::revwalk(tip, err))?;

        Ok(walk.map(move |step| match step {
            Ok(info) => Ok(CommitId::from_gix(info.id)),
            Err(err) => Err(GitError::revwalk(tip, err)),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::fixture::TempRepo;

    /// Sammelt den ganzen Walk und bricht beim ersten Defekt ab.
    fn walk(repo: &Repo, tip: CommitId) -> Result<Vec<CommitId>> {
        repo.revwalk(tip)?.collect()
    }

    #[test]
    fn linear_history_comes_back_newest_first() {
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");
        let c3 = fixture.commit("c3");

        let repo = Repo::open(fixture.path()).unwrap();
        assert_eq!(walk(&repo, c3).unwrap(), vec![c3, c2, c1]);
    }

    #[test]
    fn walk_starts_at_the_given_tip_and_ignores_newer_commits() {
        // Der Walk läuft rückwärts: Was nach `tip` kam, ist von dort aus nicht
        // erreichbar und taucht nicht auf.
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");
        fixture.commit("c3");

        let repo = Repo::open(fixture.path()).unwrap();
        assert_eq!(walk(&repo, c2).unwrap(), vec![c2, c1]);
    }

    #[test]
    fn root_commit_walks_to_itself() {
        let fixture = TempRepo::init();
        let root = fixture.commit("c1");

        let repo = Repo::open(fixture.path()).unwrap();
        assert_eq!(walk(&repo, root).unwrap(), vec![root]);
    }

    #[test]
    fn merge_history_yields_every_commit_exactly_once() {
        // c1 ──── c2 ──── merge
        //  └────── f1 ──────┘
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");
        fixture.git(&["checkout", "--quiet", "-b", "feature", &c1.to_string()]);
        let f1 = fixture.commit("f1");
        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.git(&["merge", "--quiet", "--no-ff", "-m", "merge", "feature"]);
        let merge = fixture.rev_parse("HEAD");

        let repo = Repo::open(fixture.path()).unwrap();
        let seen = walk(&repo, merge).unwrap();

        assert_eq!(seen.len(), 4, "jeder Commit genau einmal: {seen:?}");
        assert_eq!(
            seen.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([merge, c2, f1, c1])
        );
        // Über die Reihenfolge sagen wir nur das zu, was der Graph hergibt:
        // vom Tip aus los, und die Wurzel kommt zuletzt.
        assert_eq!(seen.first(), Some(&merge));
        assert_eq!(seen.last(), Some(&c1));
    }

    #[test]
    fn unknown_tip_is_an_error_not_an_empty_history() {
        // Der fail-closed-Fall: `minds fsck` darf ein fehlendes Objekt nicht
        // als „nichts zu prüfen" lesen.
        let fixture = TempRepo::init();
        fixture.commit("c1");
        let repo = Repo::open(fixture.path()).unwrap();
        let missing: CommitId = "0000000000000000000000000000000000000001".parse().unwrap();

        let err = walk(&repo, missing).unwrap_err();
        assert!(matches!(err, GitError::Revwalk { .. }));
    }

    #[test]
    fn walk_starts_at_head() {
        // Der Weg, den die CLI geht: HEAD auflösen, von dort ablaufen.
        let fixture = TempRepo::init();
        let c1 = fixture.commit("c1");
        let c2 = fixture.commit("c2");

        let repo = Repo::open(fixture.path()).unwrap();
        let head = repo.head().unwrap().commit().expect("Repo hat Commits");
        assert_eq!(walk(&repo, head).unwrap(), vec![c2, c1]);
    }
}
