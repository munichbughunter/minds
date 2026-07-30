//! Trailer aus Commit-Messages lesen: der Rückweg vom Code zur Session.
//!
//! Das hier schließt die Schleife, um die es in der Vision geht:
//!
//! ```text
//! Buggy-Zeile → git blame → Commit → Trailer → SessionId → Store → Prompt
//! ```
//!
//! Ohne diese Leserichtung wäre der Store ein Ort, in den nur geschrieben wird.
//! `minds show <commit>` und `minds why <datei>:<zeile>` (M6) setzen genau hier
//! auf, `minds fsck` ebenfalls — es läuft die Historie ab (siehe `walk.rs`) und
//! prüft für jeden gefundenen Verweis, ob er sich auflösen lässt.
//!
//! # Die Arbeitsteilung mit `minds-core`
//!
//! Geparst wird **nicht hier**. [`minds_core::Trailer`] kennt die Grammatik der
//! Trailer-Zeile und hat dafür Golden-Tests ohne Git, ohne Netz, ohne I/O.
//! Dieses Modul beschafft nur den Text und reicht ihn weiter. Die Trennung ist
//! dieselbe wie im ganzen Projekt: `core` ist reine Funktion, `git` ist I/O.
//!
//! Wer mehr als SessionIds braucht (sobald `Minds-Attribution` dazukommt),
//! nimmt [`Repo::message_of`] und ruft `Trailer::extract_all` selbst auf —
//! dafür braucht es hier keine zweite Methode.
//!
//! # Warum der Verweis das hier überlebt, was Commit-Hashes nicht überleben
//!
//! Der Trailer steht im **Text** der Message, nicht am Commit-Hash. `rebase`,
//! `squash` und `cherry-pick` erzeugen neue Hashes, nehmen die Message aber mit
//! — der Verweis wandert mit. Beim Squash konkateniert Git die Messages, die
//! Trailer der Einzel-Commits sammeln sich also, und das ist genau richtig:
//! Mehrere Sessions haben beigetragen.
//!
//! Das ist keine Theorie, sondern der Grund, warum der Verweis nicht als
//! `git note` gespeichert wird (die hängen an der SHA und bleiben beim Rewrite
//! an der alten hängen). Die Tests unten weisen beides an echtem `git` nach:
//! einmal Rebase, einmal Squash-Merge.
//!
//! # Messages sind Bytes, keine Strings
//!
//! Git speichert Commit-Messages als Bytes; das `encoding`-Feld darf alles
//! Mögliche sagen. [`Repo::message_of`] wandelt **verlustbehaftet** nach
//! UTF-8 um. Für den Zweck ist das die richtige Wahl: Ein Trailer besteht aus
//! ASCII (Schlüssel, `b3-`, Hex). Ein Umlaut in Latin-1 zwei Absätze weiter
//! oben darf den Verweis nicht unlesbar machen — und ein harter Fehler an
//! dieser Stelle hieße, eine erfasste Session zu verlieren, weil jemandes
//! Editor falsch eingestellt war.
//!
//! Umgekehrt kann die Umwandlung keinen Trailer *erfinden*: Aus kaputten Bytes
//! wird `U+FFFD`, und daran scheitert die SessionId-Grammatik.

use minds_core::{SessionId, Trailer};

use crate::error::{GitError, Result};
use crate::oid::CommitId;
use crate::repo::Repo;

impl Repo {
    /// Die vollständige Commit-Message, verlustbehaftet nach UTF-8 gewandelt.
    ///
    /// „Vollständig" heißt: Betreff *und* Rumpf, so wie sie im Commit-Objekt
    /// stehen — nicht Gits aufbereitete Zusammenfassung.
    pub fn message_of(&self, commit: CommitId) -> Result<String> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;

        let raw = object
            .message_raw()
            .map_err(|err| GitError::read_object(commit, err))?;

        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// Alle über Trailer verlinkten [`SessionId`]s eines Commits, in
    /// Auftretens-Reihenfolge.
    ///
    /// Leer, wenn der Commit keine trägt — der Normalfall für jeden von Hand
    /// geschriebenen Commit und kein Fehler.
    ///
    /// Die Reihenfolge ist die der Trailer **in der Message** und damit
    /// ausdrücklich *nicht* chronologisch: Nach einem `git merge --squash`
    /// steht die zuletzt entstandene Session vorn, nach einem flachen
    /// Rebase-Squash die erste. Wer eine zeitliche Ordnung braucht, holt sie
    /// aus den Sessions selbst, nicht aus dieser Liste.
    ///
    /// **Ohne Deduplizierung**, wie in [`minds_core::Trailer::session_ids`]:
    /// Content-adressiert ist eine doppelt genannte Id dieselbe Session, aber
    /// ob das eine Auffälligkeit ist (`fsck`) oder egal (`show`), entscheidet
    /// der Aufrufer, nicht diese Ebene.
    pub fn session_ids_of(&self, commit: CommitId) -> Result<Vec<SessionId>> {
        Ok(Trailer::session_ids(&self.message_of(commit)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::TempRepo;

    /// Eine gültige [`SessionId`] aus einem wiederholten Hex-Zeichen.
    fn id(hex: char) -> SessionId {
        format!("b3-{}", hex.to_string().repeat(64))
            .parse()
            .unwrap()
    }

    /// Die kanonische Trailer-Zeile zu einer Id — über `minds-core`, damit die
    /// Tests nicht ihre eigene Schreibweise erfinden.
    fn line(session: SessionId) -> String {
        Trailer::SessionId(session).to_string()
    }

    #[test]
    fn a_commit_without_trailers_yields_nothing() {
        let fixture = TempRepo::init();
        let commit = fixture.commit("feat: ganz normal von Hand");
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), Vec::new());
    }

    #[test]
    fn message_of_returns_subject_and_body() {
        let fixture = TempRepo::init();
        let commit = fixture.commit("feat: etwas\n\nDer Rumpf.\n");
        let repo = Repo::open(fixture.path()).unwrap();

        let message = repo.message_of(commit).unwrap();
        assert!(message.starts_with("feat: etwas"));
        assert!(message.contains("Der Rumpf."));
    }

    #[test]
    fn a_single_trailer_resolves_to_its_session() {
        let fixture = TempRepo::init();
        let session = id('a');
        let commit = fixture.commit(&format!("feat: etwas\n\n{}", line(session)));
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
    }

    #[test]
    fn several_sessions_on_one_commit_are_all_found() {
        // Mehrere Agent-Läufe haben zu einem Commit beigetragen.
        let fixture = TempRepo::init();
        let (first, second) = (id('a'), id('b'));
        let commit = fixture.commit(&format!("feat: etwas\n\n{}\n{}", line(first), line(second)));
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![first, second]);
    }

    #[test]
    fn foreign_trailers_are_ignored() {
        let fixture = TempRepo::init();
        let session = id('c');
        let commit = fixture.commit(&format!(
            "fix: etwas\n\nSigned-off-by: A <a@x.invalid>\n{}\nCo-authored-by: B <b@x.invalid>",
            line(session)
        ));
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
    }

    #[test]
    fn a_malformed_trailer_is_skipped_rather_than_fatal() {
        // Von Hand verhunzt: Der Rest der Message bleibt trotzdem auswertbar.
        let fixture = TempRepo::init();
        let good = id('d');
        let commit = fixture.commit(&format!(
            "fix: etwas\n\nMinds-Session-Id: b3-nicht-hex\n{}",
            line(good)
        ));
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![good]);
    }

    #[test]
    fn a_trailer_in_the_middle_of_the_message_is_found() {
        // Gits eigene Trailer-Logik sieht nur den letzten Absatz an. Wir
        // scannen die ganze Message — sonst gingen beim Squash alle bis auf
        // den letzten Verweis verloren.
        let fixture = TempRepo::init();
        let session = id('e');
        let commit = fixture.commit(&format!(
            "feat: etwas\n\n{}\n\nNoch ein Absatz danach.\n",
            line(session)
        ));
        let repo = Repo::open(fixture.path()).unwrap();

        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
    }

    #[test]
    fn the_reference_survives_a_rebase() {
        // Die zentrale Zusage des Entwurfs: Der Commit-Hash ändert sich, der
        // Verweis nicht. Deshalb steht er in der Message und nicht in einer
        // `git note` (die hängt an der SHA und bliebe an der alten hängen).
        let fixture = TempRepo::init();
        let session = id('f');

        fixture.write_file("a.txt", "a\n");
        fixture.commit("base");

        fixture.git(&["checkout", "--quiet", "-b", "feature"]);
        fixture.write_file("b.txt", "b\n");
        let before = fixture.commit(&format!("feat: b\n\n{}", line(session)));

        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.write_file("c.txt", "c\n");
        fixture.commit("main läuft weiter");

        fixture.git(&["checkout", "--quiet", "feature"]);
        fixture.git(&["rebase", "--quiet", "main"]);
        let after = fixture.rev_parse("HEAD");

        let repo = Repo::open(fixture.path()).unwrap();
        assert_ne!(before, after, "der Rebase hat den Hash geändert");
        assert_eq!(repo.session_ids_of(after).unwrap(), vec![session]);
    }

    #[test]
    fn a_squash_collects_the_trailers_of_all_squashed_commits() {
        // Zwei Sessions, zwei Commits, ein Squash — beide Verweise müssen im
        // Ergebnis stehen. Gegenprobe an echtem git: Die Message baut hier
        // `git merge --squash` zusammen, nicht der Test.
        //
        // ACHTUNG: `git merge --squash` schreibt die Einzel-Messages im
        // `log`-Format nach SQUASH_MSG — also mit **vier Leerzeichen
        // eingerückten** Rümpfen. Damit steht `Minds-Session-Id:` nicht mehr am
        // Zeilenanfang. Dieser Test besteht deshalb nur, wenn
        // `minds_core::Trailer` führende Leerzeichen toleriert; siehe die
        // Notiz in `parse_rejects_indented_key` dort.
        let fixture = TempRepo::init();
        let (first, second) = (id('1'), id('2'));

        fixture.write_file("a.txt", "a\n");
        fixture.commit("base");

        fixture.git(&["checkout", "--quiet", "-b", "feature"]);
        fixture.write_file("b.txt", "b\n");
        fixture.commit(&format!("feat: b\n\n{}", line(first)));
        fixture.write_file("c.txt", "c\n");
        fixture.commit(&format!("feat: c\n\n{}", line(second)));

        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.git(&["merge", "--squash", "feature"]);
        fixture.git(&["commit", "--quiet", "--no-edit"]);
        let squashed = fixture.rev_parse("HEAD");

        let repo = Repo::open(fixture.path()).unwrap();
        let found = repo.session_ids_of(squashed).unwrap();

        // Keine Zusage zur Reihenfolge: `git merge --squash` listet die Commits
        // wie `git log`, also **neuester zuerst**; ein flacher Rebase-Squash
        // dreht das um. Beides ist gültig, also prüfen wir nur, dass beide
        // Verweise genau einmal drinstehen.
        assert_eq!(
            found.len(),
            2,
            "beide Verweise müssen erhalten bleiben: {found:?}"
        );
        assert!(found.contains(&first), "{found:?}");
        assert!(found.contains(&second), "{found:?}");
    }

    #[test]
    fn the_reference_survives_a_cherry_pick() {
        let fixture = TempRepo::init();
        let session = id('9');

        fixture.write_file("a.txt", "a\n");
        fixture.commit("base");

        fixture.git(&["checkout", "--quiet", "-b", "feature"]);
        fixture.write_file("b.txt", "b\n");
        let original = fixture.commit(&format!("feat: b\n\n{}", line(session)));

        // main muss weiterlaufen, sonst bekommt der Pick denselben Elter, denselben
        // Baum und (weil das Fixture Autor, Committer und Datum festnagelt) exakt
        // denselben Hash — der Test prüfte dann nichts.
        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.write_file("c.txt", "c\n");
        fixture.commit("main läuft weiter");

        fixture.git(&["cherry-pick", &original.to_string()]);
        let picked = fixture.rev_parse("HEAD");

        let repo = Repo::open(fixture.path()).unwrap();
        assert_ne!(original, picked);
        assert_eq!(repo.session_ids_of(picked).unwrap(), vec![session]);
    }

    #[test]
    fn a_message_that_is_not_utf8_still_yields_its_trailer() {
        // Latin-1-Umlaut im Betreff, ASCII-Trailer darunter. Ein falsch
        // eingestellter Editor darf keine Session verlieren.
        let fixture = TempRepo::init();
        let session = id('7');

        let mut message = b"fix: \xc4nderung an der Br\xfccke\n\n".to_vec();
        message.extend_from_slice(line(session).as_bytes());
        message.push(b'\n');

        let commit = fixture.commit_with_raw_message(&message);
        let repo = Repo::open(fixture.path()).unwrap();

        // Die Zusage dieses Moduls: kein harter Fehler, der Trailer bleibt
        // auflösbar. Das gilt unabhängig davon, was Git mit den Bytes tut.
        let decoded = repo.message_of(commit).unwrap();
        assert_eq!(repo.session_ids_of(commit).unwrap(), vec![session]);
        assert!(decoded.contains("Minds-Session-Id"));

        // Ob überhaupt eine verlustbehaftete Wandlung stattfindet, entscheidet
        // Git: Es kann die Bytes unangetastet ablegen (dann sehen wir U+FFFD)
        // oder die Message vorher nach UTF-8 wandeln (dann nicht). Statt eine
        // der beiden Möglichkeiten anzunehmen, sehen wir im Commit-Objekt nach.
        let stored = fixture.git_bytes(&["cat-file", "commit", "HEAD"]);
        if stored.contains(&0xc4) {
            assert!(
                decoded.contains('\u{fffd}'),
                "Git hat die Bytes behalten, also muss die Wandlung ersetzen"
            );
        } else {
            assert!(
                !decoded.contains('\u{fffd}'),
                "Git hat die Message gewandelt, es kann nichts zu ersetzen geben"
            );
        }
    }

    #[test]
    fn reading_an_unknown_commit_fails() {
        let fixture = TempRepo::init();
        fixture.commit("base");
        let repo = Repo::open(fixture.path()).unwrap();
        let missing: CommitId = "0000000000000000000000000000000000000001".parse().unwrap();

        let err = repo.session_ids_of(missing).unwrap_err();
        assert!(matches!(err, GitError::ReadObject { .. }), "{err}");
    }
}
