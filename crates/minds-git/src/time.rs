//! Der Zeitpunkt eines Commits — für alles, was Sessions und Commits zeitlich
//! nebeneinanderlegt.
//!
//! Gelesen wird die **Autor-Zeit**, nicht die Committer-Zeit: Sie ist es, die
//! das heuristische Matching in `minds-capture` gegen das Session-Fenster
//! hält (`git log --format=%at`), und wer die Heuristik nachrechnen will,
//! muss dieselbe Uhr lesen. Ein Rebase verschiebt die Committer-Zeit, die
//! Autor-Zeit bleibt bei der Arbeit.

use crate::error::{GitError, Result};
use crate::oid::CommitId;
use crate::repo::Repo;

impl Repo {
    /// Die Autor-Zeit eines Commits in Sekunden seit Unix-Epoch (UTC).
    pub fn commit_time(&self, commit: CommitId) -> Result<i64> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;
        let time = object
            .author()
            .map_err(|err| GitError::read_object(commit, err))?
            .time()
            .map_err(|err| GitError::read_object(commit, err))?;
        Ok(time.seconds)
    }
}

#[cfg(test)]
mod tests {
    use crate::fixture::TempRepo;

    #[test]
    fn author_time_is_read_in_epoch_seconds() {
        let repo = TempRepo::init();
        let commit = repo.commit("erster");
        let expected: i64 = repo
            .git(&["log", "-1", "--format=%at"])
            .trim()
            .parse()
            .unwrap();
        let opened = crate::Repo::open(repo.path()).unwrap();
        assert_eq!(opened.commit_time(commit).unwrap(), expected);
    }
}
