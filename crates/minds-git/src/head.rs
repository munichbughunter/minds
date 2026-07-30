//! HEAD auflösen: [`Head`] und [`Repo::head`].
//!
//! HEAD hat in Git drei Zustände, und alle drei kommen in echten Repos vor.
//! Dieses Modul macht sie zu drei Enum-Varianten statt zu einem
//! `Option<ObjectId>` mit Fußnote:
//!
//! | Zustand | Wann | Für Minds |
//! |---|---|---|
//! | [`Head::Branch`] | Normalfall | Startpunkt für Revwalk und Trailer |
//! | [`Head::Detached`] | Rebase, Bisect, `git checkout <sha>` | funktioniert genauso — nur ohne Branch-Namen |
//! | [`Head::Unborn`] | frisch `git init`, noch kein Commit | **kein Fehler** |
//!
//! Der ungeborene HEAD ist der Grund für das Enum. Ein frisch initialisiertes
//! Repo hat einen HEAD, der auf `refs/heads/main` zeigt — nur existiert dieser
//! Ref noch nicht. Für `minds fsck` ist das die korrekte Antwort „nichts zu
//! prüfen", nicht ein Abbruch; und `minds init` läuft genau in diesem Zustand.
//! Wäre der Fall ein `Err`, würde jeder Aufrufer ihn wieder herausfiltern
//! müssen — und einer würde es vergessen.
//!
//! # Peeling
//!
//! HEAD wird bis auf einen Commit „geschält". Zeigt HEAD auf ein annotiertes
//! Tag (nach `git checkout v1.0` möglich), liefert [`Head::Detached`] den
//! Commit dahinter, nicht das Tag-Objekt. Minds interessiert die Historie,
//! nicht die Verpackung.

use crate::error::Result;
use crate::oid::CommitId;
use crate::repo::Repo;

/// Der aufgelöste Zustand von HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// HEAD zeigt auf einen Branch, der bereits einen Commit hat.
    Branch {
        /// Voller Ref-Name, z. B. `refs/heads/main`.
        ///
        /// Bewusst der volle Name und nicht `main`: Kurznamen sind mehrdeutig
        /// (ein Branch `context` und ein Tag `context` kürzen sich gleich).
        /// Kürzen ist Präsentation und gehört in die CLI.
        name: String,
        /// Der Commit, auf den der Branch zeigt.
        commit: CommitId,
    },

    /// HEAD zeigt direkt auf einen Commit, ohne Branch (Rebase, Bisect,
    /// `git checkout <sha>`).
    Detached {
        /// Der ausgecheckte Commit.
        commit: CommitId,
    },

    /// HEAD zeigt auf einen Branch, den es noch nicht gibt — frisch
    /// initialisiertes Repo ohne Commit.
    Unborn {
        /// Voller Ref-Name des Branches, der beim ersten Commit entstünde.
        name: String,
    },
}

impl Head {
    /// Der Commit, auf dem HEAD steht — `None` genau dann, wenn das Repo noch
    /// keinen hat.
    pub fn commit(&self) -> Option<CommitId> {
        match self {
            Head::Branch { commit, .. } | Head::Detached { commit } => Some(*commit),
            Head::Unborn { .. } => None,
        }
    }

    /// Der volle Ref-Name des Branches — `None` im detached-Zustand.
    ///
    /// Auch ein [`Head::Unborn`] hat einen Namen: den Branch, der beim ersten
    /// Commit entstünde.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch { name, .. } | Head::Unborn { name } => Some(name),
            Head::Detached { .. } => None,
        }
    }

    /// Ob das Repository noch keinen Commit hat.
    pub fn is_unborn(&self) -> bool {
        matches!(self, Head::Unborn { .. })
    }
}

impl Repo {
    /// Löst HEAD auf.
    ///
    /// Ein ungeborener HEAD ist **kein Fehler**, sondern [`Head::Unborn`];
    /// siehe Modul-Doku. Ein Fehler entsteht nur, wenn HEAD selbst unlesbar ist
    /// oder sich nicht bis auf einen Commit schälen lässt — also bei einem
    /// defekten Repository.
    pub fn head(&self) -> Result<Head> {
        let head = self.gix().head().map_err(|err| self.err_head(err))?;

        // Zuerst den Namen sichern: `into_peeled_id` verbraucht `head`.
        // `referent_name` ist `None` genau im detached-Zustand — auch der
        // ungeborene HEAD benennt seinen Branch.
        let referent = head.referent_name().map(|name| name.as_bstr().to_string());

        if head.is_unborn() {
            return match referent {
                Some(name) => Ok(Head::Unborn { name }),
                // Kann Git so nicht erzeugen; wir raten trotzdem nicht.
                None => Err(self.err_head("HEAD ist ungeboren, benennt aber keinen Branch")),
            };
        }

        let commit = CommitId::from_gix(
            head.into_peeled_id()
                .map_err(|err| self.err_head(err))?
                .detach(),
        );

        Ok(match referent {
            Some(name) => Head::Branch { name, commit },
            None => Head::Detached { commit },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    #[test]
    fn fresh_repository_has_an_unborn_head() {
        let fixture = TempRepo::init();
        let head = Repo::open(fixture.path()).unwrap().head().unwrap();

        assert_eq!(
            head,
            Head::Unborn {
                name: "refs/heads/main".to_owned(),
            }
        );
        assert!(head.is_unborn());
        assert_eq!(head.commit(), None);
        assert_eq!(head.branch(), Some("refs/heads/main"));
    }

    #[test]
    fn head_resolves_branch_and_commit() {
        let fixture = TempRepo::init();
        let commit = fixture.commit("erster Commit");

        let head = Repo::open(fixture.path()).unwrap().head().unwrap();
        assert_eq!(
            head,
            Head::Branch {
                name: "refs/heads/main".to_owned(),
                commit,
            }
        );
        assert_eq!(head.commit(), Some(commit));
        assert!(!head.is_unborn());
    }

    #[test]
    fn head_follows_the_branch_forward() {
        let fixture = TempRepo::init();
        fixture.commit("c1");
        let second = fixture.commit("c2");

        let head = Repo::open(fixture.path()).unwrap().head().unwrap();
        assert_eq!(head.commit(), Some(second));
    }

    #[test]
    fn detached_head_has_a_commit_but_no_branch() {
        // Der Zustand mitten im Rebase — Minds muss dort arbeiten können,
        // denn genau dort werden Trailer nachgerüstet.
        let fixture = TempRepo::init();
        let first = fixture.commit("c1");
        fixture.commit("c2");
        fixture.git(&["checkout", "--quiet", "--detach", &first.to_string()]);

        let head = Repo::open(fixture.path()).unwrap().head().unwrap();
        assert_eq!(head, Head::Detached { commit: first });
        assert_eq!(head.branch(), None);
        assert!(!head.is_unborn());
    }

    #[test]
    fn head_on_a_second_branch_reports_its_full_name() {
        let fixture = TempRepo::init();
        fixture.commit("c1");
        fixture.git(&["checkout", "--quiet", "-b", "feature/minds"]);
        let commit = fixture.commit("c2");

        let head = Repo::open(fixture.path()).unwrap().head().unwrap();
        assert_eq!(
            head,
            Head::Branch {
                name: "refs/heads/feature/minds".to_owned(),
                commit,
            }
        );
    }
}
