//! Trailer nachrüsten: HEAD durch denselben Commit mit erweiterter Message
//! ersetzen.
//!
//! # Zwei Wege, wie ein Trailer an einen Production-Commit kommt
//!
//! 1. **Vor dem Commit** — die Message wird als Text erweitert, bevor Git das
//!    Objekt baut ([`minds_core::Trailer::append_all`], kein I/O, keine
//!    Historie umgeschrieben). Das ist der `prepare-commit-msg`-Weg aus M6 und
//!    der bevorzugte: Er kostet nichts und hinterlässt keine Spur.
//! 2. **Nach dem Commit** — dieses Modul. Für den `post-commit`-Hook und für
//!    das Nachrüsten von Hand, wenn `minds capture` erst nach dem Commit lief.
//!
//! Für den zweiten Weg gibt es keine sanfte Variante: Die Message ist Teil des
//! Commit-Objekts und geht in dessen Hash ein. Wer sie ändert, erzeugt einen
//! neuen Commit — das tut `git commit --amend` genauso. Eine `git note` wäre
//! die Alternative, hängt aber an der SHA und bliebe beim ersten `rebase` am
//! alten Commit kleben; genau deshalb steht der Verweis in der Message
//! (Architektur-Prinzip 1 im Plan).
//!
//! # Nur HEAD
//!
//! [`Repo::amend_head_with_sessions`] fasst ausschließlich den Commit an, auf
//! dem HEAD steht. Einen Commit weiter unten umzuschreiben zöge jeden
//! Nachfahren mit — das ist `filter-branch`-Gebiet und hat in einem Hook nichts
//! zu suchen. Der Anwendungsfall ist „gerade eben committet"; alles Ältere
//! bekommt seinen Trailer über einen regulären interaktiven Rebase, bei dem die
//! Messages ohnehin durch die Hand des Nutzers gehen.
//!
//! # Was sich ändert: die Message. Sonst nichts.
//!
//! Baum, Eltern, Autor, Committer, Encoding und alle Extra-Header werden
//! unverändert übernommen. Das ist strenger als `git commit --amend`, das den
//! Index mitnimmt und einen frischen Committer-Zeitstempel setzt. Zwei Gründe:
//!
//! - Ein nachgerüsteter Trailer ist ein **mechanischer Verweis, kein
//!   Autorschafts-Ereignis**. Wer den Commit geschrieben hat und wann, hat sich
//!   dadurch nicht geändert.
//! - Der Unterschied zwischen Vorher und Nachher soll aus genau einer Zeile
//!   bestehen. Das macht den Vorgang prüfbar — und ist als Test formuliert
//!   (`nothing_but_the_message_changes`), nicht bloß als Zusage.
//!
//! Der Commit-Hash ändert sich trotzdem. Das ist der Preis und der Grund für
//! die Warnung unten.
//!
//! # Zwei Fälle, in denen nicht angefasst wird
//!
//! - **Signierte Commits.** Die Signatur deckt die Message ab; jede Ergänzung
//!   entwertet sie. Minds macht die Signatur eines anderen weder still kaputt
//!   noch wirft es sie weg — es lehnt ab ([`GitError::SignedCommit`]) und
//!   verweist damit auf Weg 1, bei dem der Trailer *vor* der Signatur
//!   entsteht.
//! - **Messages, die kein UTF-8 sind.** Gelesen wird tolerant und verlustbehaftet
//!   ([`Repo::message_of`]); zurückgeschrieben würde diese Wandlung die Bytes
//!   des Nutzers durch `U+FFFD` ersetzen. Also lieber gar nicht
//!   ([`GitError::MessageNotUtf8`]).
//!
//! # Compare-and-Swap und die Warnung
//!
//! Der Ref-Wechsel setzt denselben Erwartungswert wie `refs.rs`: Hat sich HEAD
//! zwischen Lesen und Schreiben bewegt, schlägt der Vorgang mit
//! [`GitError::RefRaced`] fehl, statt den fremden Stand zu überschreiben. Und
//! wie jedes Umschreiben von Historie gehört auch dieses nur auf **noch nicht
//! veröffentlichte** Commits — nach einem `push` ist der alte Hash bei anderen,
//! und ein `--force-with-lease` ist eine Entscheidung des Menschen, nicht eines
//! Hooks. Der alte Commit bleibt über den Reflog erreichbar.

use minds_core::{SessionId, Trailer};

use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use crate::error::{GitError, Result, Source};
use crate::oid::CommitId;
use crate::repo::Repo;

/// Was [`Repo::amend_head_with_sessions`] an HEAD bewirkt hat.
///
/// Das Gegenstück zu [`RefUpdate`](crate::RefUpdate) für den Kontext-Ref: Auch
/// hier ist der „nichts getan"-Fall eine eigene Variante und kein stiller
/// Erfolg — ein Hook, der zweimal läuft, soll das sagen können.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailerUpdate {
    /// Alle Trailer standen schon in der Message — nichts geschrieben, HEAD
    /// steht unverändert auf diesem Commit.
    Unchanged(CommitId),

    /// HEAD wurde durch einen neuen Commit mit den fehlenden Trailern ersetzt.
    Amended {
        /// Der Commit, der vorher an HEAD stand. Über den Reflog weiterhin
        /// erreichbar.
        before: CommitId,
        /// Der Commit, der ihn ersetzt hat.
        after: CommitId,
    },
}

impl TrailerUpdate {
    /// Der Commit, auf den HEAD jetzt zeigt.
    pub fn commit(&self) -> CommitId {
        match self {
            TrailerUpdate::Unchanged(commit) => *commit,
            TrailerUpdate::Amended { after, .. } => *after,
        }
    }

    /// Ob dabei ein Commit umgeschrieben wurde.
    pub fn rewrote_head(&self) -> bool {
        matches!(self, TrailerUpdate::Amended { .. })
    }
}

impl Repo {
    /// Rüstet die Trailer zu `sessions` an HEAD nach.
    ///
    /// Idempotent: Was schon in der Message steht, wird nicht wiederholt;
    /// fehlt nichts, entsteht kein neuer Commit
    /// ([`TrailerUpdate::Unchanged`]). Ein leeres `sessions` ist damit ein
    /// erlaubter Leerlauf und kein Fehler.
    ///
    /// Reihenfolge der Schritte: erst das neue Commit-Objekt schreiben, dann
    /// den Ref bewegen. Bricht der zweite Schritt ab, liegt ein unerreichbares
    /// Objekt in der Datenbank, das `git gc` einsammelt — verloren ist nichts.
    ///
    /// # Fehler
    ///
    /// - [`GitError::NothingToAmend`] — HEAD hat noch keinen Commit.
    /// - [`GitError::SignedCommit`] — der Commit ist signiert.
    /// - [`GitError::MessageNotUtf8`] — die Message ist kein gültiges UTF-8.
    /// - [`GitError::RefRaced`] — HEAD hat sich zwischenzeitlich bewegt.
    pub fn amend_head_with_sessions(&self, sessions: &[SessionId]) -> Result<TrailerUpdate> {
        let trailers: Vec<Trailer> = sessions.iter().copied().map(Trailer::SessionId).collect();

        let Some(before) = self.head()?.commit() else {
            return Err(GitError::nothing_to_amend(self.git_dir().to_path_buf()));
        };

        let message = self.message_utf8(before)?;
        let extended = Trailer::append_all(&message, &trailers);
        if extended == message {
            return Ok(TrailerUpdate::Unchanged(before));
        }

        let after = self.commit_with_message(before, &extended)?;
        self.move_head(before, after)?;

        Ok(TrailerUpdate::Amended { before, after })
    }

    /// Die Message eines Commits, **streng** nach UTF-8 gewandelt.
    ///
    /// Das Gegenstück zu [`Repo::message_of`], das verlustbehaftet wandelt: Zum
    /// Lesen ist `U+FFFD` die richtige Antwort, zum Zurückschreiben wäre es
    /// Datenverlust.
    fn message_utf8(&self, commit: CommitId) -> Result<String> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;

        let raw = object
            .message_raw()
            .map_err(|err| GitError::read_object(commit, err))?;

        String::from_utf8(raw.to_vec()).map_err(|_| GitError::message_not_utf8(commit))
    }

    /// Schreibt `commit` mit neuer Message noch einmal in die Objektdatenbank
    /// und gibt die Id des neuen Objekts zurück. Rührt keinen Ref an.
    fn commit_with_message(&self, commit: CommitId, message: &str) -> Result<CommitId> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;

        let decoded = object
            .decode()
            .map_err(|err| GitError::read_object(commit, err))?;

        // Die Signatur deckt die Message ab — siehe Modul-Doku. `gpgsig` und
        // `gpgsig-sha256` (SHA-256-Repos) heißen beide so am Anfang.
        if let Some((header, _)) = decoded
            .extra_headers
            .iter()
            .find(|(name, _)| name.starts_with(b"gpgsig"))
        {
            return Err(GitError::signed_commit(commit, header.to_string()));
        }

        let rewritten = gix::objs::Commit {
            tree: decoded.tree(),
            parents: decoded.parents().collect(),
            author: owned_signature(decoded.author(), commit)?,
            committer: owned_signature(decoded.committer(), commit)?,
            encoding: decoded.encoding.map(ToOwned::to_owned),
            message: message.into(),
            extra_headers: decoded
                .extra_headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone().into_owned()))
                .collect(),
        };

        let id = self
            .gix()
            .write_object(&rewritten)
            .map_err(GitError::write_object)?
            .detach();

        Ok(CommitId::from_gix(id))
    }

    /// Bewegt HEAD von `before` auf `after` — mit Compare-and-Swap gegen
    /// `before`.
    ///
    /// `deref: true` lässt die Transaktion dem symbolischen HEAD folgen: Steht
    /// er auf einem Branch, bewegt sich der Branch (wie bei `git commit`);
    /// ist er detached, bewegt sich HEAD selbst. Genau das tut auch
    /// `git commit --amend` — und es ist der Grund, warum dieser Helfer im
    /// Rebase funktioniert, wo es keinen Branch gibt.
    fn move_head(&self, before: CommitId, after: CommitId) -> Result<()> {
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    // Der Reflog ist die Rückfahrkarte: Er nennt den Vorgang
                    // beim Namen und hält den alten Commit erreichbar.
                    message: "minds: Session-Trailer nachgerüstet".into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(before.to_gix())),
                new: Target::Object(after.to_gix()),
            },
            name: "HEAD".try_into().expect("HEAD ist ein gültiger Ref-Name"),
            deref: true,
        };

        match self.gix().edit_reference(edit) {
            Ok(_) => Ok(()),
            Err(err) => {
                // War es ein Wettlauf? Nachsehen statt in gix' Fehlervarianten
                // raten — dieselbe Linie wie in `refs.rs`.
                let current = self.head()?.commit();
                if current == Some(before) {
                    Err(GitError::commit("HEAD", err))
                } else {
                    Err(GitError::ref_raced("HEAD", Some(before), current))
                }
            }
        }
    }
}

/// Übernimmt Autor bzw. Committer aus dem alten Commit.
///
/// Der Weg dorthin ist zweimal fehlbar, und beide Male steckt derselbe Grund
/// dahinter — gix hält so lange rohe Bytes, wie es geht: `CommitRef::author`
/// ist der unveränderte Byte-Bereich aus dem Objekt, der gleichnamige Accessor
/// zerlegt ihn in Name, E-Mail und Zeitangabe, und auch dort bleibt die Zeit
/// zunächst Text. Erst `to_owned` liest sie.
///
/// Scheitert einer der beiden Schritte, ist die Signatur im *alten* Commit
/// kaputt; dann wird nichts umgeschrieben, statt einen Zeitstempel zu erfinden.
///
/// Der generische Fehlertyp hält gix aus der Signatur heraus — dieselbe Linie
/// wie in `error.rs`, nur eine Etage tiefer.
///
/// Damit läuft die Signatur durch einen Decode-Encode-Zyklus. Dass dabei
/// dieselben Bytes herauskommen, ist nicht angenommen, sondern geprüft:
/// `nothing_but_the_message_changes` vergleicht den Kopf des Commit-Objekts vor
/// und nach dem Nachrüsten.
fn owned_signature<E: Into<Source>>(
    signature: std::result::Result<gix::actor::SignatureRef<'_>, E>,
    commit: CommitId,
) -> Result<gix::actor::Signature> {
    signature
        .map_err(|err| GitError::read_object(commit, err))?
        .to_owned()
        .map_err(|err| GitError::read_object(commit, err))
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

    /// Ein Repo mit einer Datei und einem Commit.
    fn repo_with_commit(message: &str) -> (TempRepo, Repo, CommitId) {
        let fixture = TempRepo::init();
        fixture.write_file("src/retry.rs", "fn retry() {}\n");
        let commit = fixture.commit(message);
        let repo = Repo::open(fixture.path()).unwrap();
        (fixture, repo, commit)
    }

    /// Der Kopf eines Commit-Objekts: alles vor der Leerzeile, hinter der die
    /// Message beginnt — also Baum, Eltern, Autor, Committer, Extra-Header.
    fn header_of(fixture: &TempRepo, rev: &str) -> String {
        let object = fixture.git(&["cat-file", "commit", rev]);
        object
            .split("\n\n")
            .next()
            .expect("ein Commit-Objekt hat einen Kopf")
            .to_owned()
    }

    #[test]
    fn a_commit_without_a_trailer_gets_one() {
        let (fixture, repo, before) = repo_with_commit("feat: Retry-Backoff verlängert");
        let session = id('a');

        let update = repo.amend_head_with_sessions(&[session]).unwrap();

        assert!(update.rewrote_head());
        assert!(matches!(update, TrailerUpdate::Amended { .. }));
        assert_ne!(update.commit(), before, "der Hash muss sich ändern");
        // Gegenprobe mit echtem git: HEAD steht auf dem neuen Commit …
        assert_eq!(fixture.rev_parse("HEAD"), update.commit());
        // … und der Trailer ist über die normale Leseseite auflösbar.
        assert_eq!(repo.session_ids_of(update.commit()).unwrap(), vec![session]);
    }

    #[test]
    fn amending_with_the_same_session_twice_writes_nothing() {
        // Der Hook läuft zweimal, oder jemand ruft `minds capture` erneut auf.
        let (fixture, repo, _) = repo_with_commit("feat: etwas");
        let first = repo.amend_head_with_sessions(&[id('a')]).unwrap();
        let again = repo.amend_head_with_sessions(&[id('a')]).unwrap();

        assert_eq!(again, TrailerUpdate::Unchanged(first.commit()));
        assert!(!again.rewrote_head());
        assert_eq!(fixture.rev_parse("HEAD"), first.commit());
    }

    #[test]
    fn amending_with_no_sessions_is_a_no_op() {
        let (fixture, repo, before) = repo_with_commit("feat: etwas");

        let update = repo.amend_head_with_sessions(&[]).unwrap();

        assert_eq!(update, TrailerUpdate::Unchanged(before));
        assert_eq!(fixture.rev_parse("HEAD"), before);
    }

    #[test]
    fn nothing_but_the_message_changes() {
        // Die zentrale Zusage dieses Moduls, gegen das echte Commit-Objekt
        // geprüft: Baum, Eltern, Autor und Committer stehen hinterher
        // unverändert da — Byte für Byte.
        let (fixture, repo, _) = repo_with_commit("feat: etwas\n\nMit Rumpf.\n");
        let before = header_of(&fixture, "HEAD");

        repo.amend_head_with_sessions(&[id('a')]).unwrap();

        assert_eq!(header_of(&fixture, "HEAD"), before);
    }

    #[test]
    fn the_body_of_the_message_survives() {
        let (fixture, repo, _) = repo_with_commit("fix: etwas\n\nDer Backoff war zu kurz.\n");

        repo.amend_head_with_sessions(&[id('a')]).unwrap();

        let message = fixture.git(&["log", "-1", "--format=%B"]);
        assert!(message.starts_with("fix: etwas"), "{message:?}");
        assert!(message.contains("Der Backoff war zu kurz."), "{message:?}");
    }

    #[test]
    fn the_branch_moves_along_with_head() {
        let (fixture, repo, _) = repo_with_commit("feat: etwas");

        let update = repo.amend_head_with_sessions(&[id('a')]).unwrap();

        assert_eq!(fixture.rev_parse("refs/heads/main"), update.commit());
        // HEAD zeigt weiterhin auf den Branch, ist also nicht detached.
        assert_eq!(repo.head().unwrap().branch(), Some("refs/heads/main"));
    }

    #[test]
    fn a_detached_head_is_amended_too() {
        // Der Zustand mitten im Rebase — genau dort wird nachgerüstet.
        let (fixture, repo, first) = repo_with_commit("feat: eins");
        fixture.write_file("b.txt", "b\n");
        fixture.commit("feat: zwei");
        fixture.git(&["checkout", "--quiet", "--detach", &first.to_string()]);
        let main_before = fixture.rev_parse("refs/heads/main");

        let update = repo.amend_head_with_sessions(&[id('a')]).unwrap();

        assert_eq!(fixture.rev_parse("HEAD"), update.commit());
        assert!(
            repo.head().unwrap().branch().is_none(),
            "HEAD bleibt detached"
        );
        assert_eq!(
            fixture.rev_parse("refs/heads/main"),
            main_before,
            "main bleibt stehen"
        );
    }

    #[test]
    fn several_sessions_become_several_trailers() {
        // Mehrere Agent-Läufe haben zu einem Commit beigetragen.
        let (_fixture, repo, _) = repo_with_commit("feat: großer Wurf");
        let (first, second) = (id('a'), id('b'));

        let update = repo.amend_head_with_sessions(&[first, second]).unwrap();

        assert_eq!(
            repo.session_ids_of(update.commit()).unwrap(),
            vec![first, second]
        );
    }

    #[test]
    fn a_missing_session_is_added_next_to_the_one_that_is_there() {
        let (_fixture, repo, _) = repo_with_commit("feat: etwas");
        repo.amend_head_with_sessions(&[id('a')]).unwrap();

        let update = repo.amend_head_with_sessions(&[id('a'), id('b')]).unwrap();

        assert!(update.rewrote_head());
        assert_eq!(
            repo.session_ids_of(update.commit()).unwrap(),
            vec![id('a'), id('b')]
        );
    }

    #[test]
    fn an_existing_trailer_block_is_extended_not_pushed_apart() {
        // Fremde Trailer und unserer gehören in denselben Absatz — sonst läse
        // Gits eigene Trailer-Logik nur noch unseren.
        let (fixture, repo, _) = repo_with_commit("fix: etwas\n\nSigned-off-by: A <a@x.invalid>");
        let session = id('c');

        repo.amend_head_with_sessions(&[session]).unwrap();

        let message = fixture.git(&["log", "-1", "--format=%B"]);
        assert!(
            message.contains(&format!(
                "Signed-off-by: A <a@x.invalid>\n{}",
                line(session)
            )),
            "kein gemeinsamer Absatz: {message:?}"
        );
    }

    #[test]
    fn the_amend_takes_nothing_from_the_index() {
        // Anders als `git commit --amend`: Was gestaged ist, bleibt gestaged.
        let (fixture, repo, _) = repo_with_commit("feat: etwas");
        fixture.write_file("nachtraeglich.txt", "noch nicht committet\n");

        let update = repo.amend_head_with_sessions(&[id('a')]).unwrap();

        let staged = fixture.git(&["diff", "--cached", "--name-only"]);
        assert!(
            staged.contains("nachtraeglich.txt"),
            "die Datei ist aus dem Index gerutscht: {staged:?}"
        );
        let files = fixture.git(&[
            "show",
            "--name-only",
            "--format=",
            &update.commit().to_string(),
        ]);
        assert!(
            !files.contains("nachtraeglich.txt"),
            "die Datei ist in den Commit gerutscht: {files:?}"
        );
    }

    #[test]
    fn the_amend_is_visible_in_the_reflog() {
        // Ein umgeschriebener Commit muss nachvollziehbar bleiben: Der alte
        // steht im Reflog, und der Eintrag nennt den Vorgang.
        let (fixture, repo, before) = repo_with_commit("feat: etwas");

        repo.amend_head_with_sessions(&[id('a')]).unwrap();

        let reflog = fixture.git(&["reflog", "--format=%H %gs"]);
        assert!(
            reflog.contains("minds"),
            "Vorgang nicht benannt: {reflog:?}"
        );
        assert!(
            reflog.contains(&before.to_string()),
            "der alte Commit fehlt: {reflog:?}"
        );
    }

    #[test]
    fn the_trailer_survives_a_rebase_after_the_amend() {
        // Die Zusage aus dem Plan, hier über den Amend-Pfad: Der Hash ändert
        // sich zweimal, der Verweis übersteht beides.
        let fixture = TempRepo::init();
        let session = id('f');
        fixture.write_file("a.txt", "a\n");
        fixture.commit("base");

        fixture.git(&["checkout", "--quiet", "-b", "feature"]);
        fixture.write_file("b.txt", "b\n");
        fixture.commit("feat: b");

        let repo = Repo::open(fixture.path()).unwrap();
        repo.amend_head_with_sessions(&[session]).unwrap();

        fixture.git(&["checkout", "--quiet", "main"]);
        fixture.write_file("c.txt", "c\n");
        fixture.commit("main läuft weiter");

        fixture.git(&["checkout", "--quiet", "feature"]);
        fixture.git(&["rebase", "--quiet", "main"]);

        let after = fixture.rev_parse("HEAD");
        assert_eq!(repo.session_ids_of(after).unwrap(), vec![session]);
    }

    #[test]
    fn an_unborn_head_has_nothing_to_amend() {
        let fixture = TempRepo::init();
        let repo = Repo::open(fixture.path()).unwrap();

        let err = repo.amend_head_with_sessions(&[id('a')]).unwrap_err();
        assert!(matches!(err, GitError::NothingToAmend { .. }), "{err}");
    }

    #[test]
    fn a_signed_commit_is_refused() {
        // `git commit -S` bräuchte einen echten Schlüssel; für die Frage „fasst
        // Minds signierte Commits an?" reicht der Header. Das Objekt wird
        // deshalb von Hand gebaut — `\x20` ist das führende Leerzeichen der
        // Fortsetzungszeilen, das die Zeilenfortsetzung im Quelltext sonst
        // schluckt.
        let fixture = TempRepo::init();
        fixture.write_file("a.txt", "a\n");
        let parent = fixture.commit("base");
        let tree = fixture.hash("HEAD^{tree}");

        let object = format!(
            "tree {tree}\n\
             parent {parent}\n\
             author Minds Test <test@example.invalid> 1704067200 +0000\n\
             committer Minds Test <test@example.invalid> 1704067200 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n\
             \x20nicht echt, aber an der richtigen Stelle\n\
             \x20-----END PGP SIGNATURE-----\n\
             \n\
             fix: signiert\n"
        );
        let signed = fixture.write_raw_object("commit", object.as_bytes());
        fixture.git(&["update-ref", "refs/heads/main", &signed]);

        let repo = Repo::open(fixture.path()).unwrap();
        let err = repo.amend_head_with_sessions(&[id('a')]).unwrap_err();

        assert!(matches!(err, GitError::SignedCommit { .. }), "{err}");
        assert_eq!(
            fixture.hash("HEAD"),
            signed,
            "der signierte Commit muss stehen bleiben"
        );
    }

    #[test]
    fn a_message_that_is_not_utf8_is_left_alone() {
        // Latin-1-Umlaut im Betreff. Ob die Bytes so im Objekt landen,
        // entscheidet Git (siehe `trailer.rs`) — also erst nachsehen, dann
        // prüfen.
        let fixture = TempRepo::init();
        fixture.write_file("a.txt", "a\n");
        let mut message = b"fix: \xc4nderung an der Br\xfccke".to_vec();
        message.push(b'\n');
        let before = fixture.commit_with_raw_message(&message);
        let repo = Repo::open(fixture.path()).unwrap();

        if fixture
            .git_bytes(&["cat-file", "commit", "HEAD"])
            .contains(&0xc4)
        {
            let err = repo.amend_head_with_sessions(&[id('a')]).unwrap_err();
            assert!(matches!(err, GitError::MessageNotUtf8 { .. }), "{err}");
            assert_eq!(
                fixture.rev_parse("HEAD"),
                before,
                "der Commit muss stehen bleiben"
            );
        } else {
            // Git hat die Message gewandelt — dann ist sie UTF-8 und es gibt
            // keinen Grund abzulehnen.
            let update = repo.amend_head_with_sessions(&[id('a')]).unwrap();
            assert_eq!(repo.session_ids_of(update.commit()).unwrap(), vec![id('a')]);
        }
    }

    #[test]
    fn a_head_that_moved_underneath_us_is_reported_as_a_race() {
        // Ohne echte Nebenläufigkeit: Wir behaupten, HEAD stünde noch auf dem
        // ersten Commit — so, als wäre uns ein zweiter Lauf zuvorgekommen.
        let (fixture, repo, first) = repo_with_commit("feat: eins");
        fixture.write_file("b.txt", "b\n");
        let second = fixture.commit("feat: zwei");

        let err = repo.move_head(first, second).unwrap_err();

        assert!(matches!(err, GitError::RefRaced { .. }), "{err}");
        assert_eq!(
            fixture.rev_parse("HEAD"),
            second,
            "HEAD bleibt unangetastet"
        );
    }
}
