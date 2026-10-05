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
    /// - [`GitError::RefRaced`] — HEAD hat sich zwischenzeitlich bewegt (auch
    ///   wenn nichts anzuhängen war: dann steht HEAD nicht mehr auf dem
    ///   gelesenen Commit, und `Unchanged` wäre eine falsche Auskunft).
    pub fn amend_head_with_sessions(&self, sessions: &[SessionId]) -> Result<TrailerUpdate> {
        let Some(before) = self.head()?.commit() else {
            return Err(GitError::nothing_to_amend(self.git_dir().to_path_buf()));
        };
        self.amend_commit_with_sessions(before, sessions)
    }

    /// Wie [`Repo::amend_head_with_sessions`], aber gegen einen **vom
    /// Aufrufer beobachteten** HEAD-Commit: Der Compare-and-Swap prüft gegen
    /// genau `before`, nicht gegen einen zweiten Blick auf HEAD.
    ///
    /// Für Aufrufer, die HEAD erst prüfen und dann trailern (Wächter-Commit,
    /// zwei Schreiber, EA-06d): Zwischen Prüfung und Amend darf HEAD nicht
    /// unbemerkt auf einen anderen Commit springen.
    ///
    /// # Fehler
    ///
    /// Wie [`Repo::amend_head_with_sessions`]; [`GitError::RefRaced`] auch
    /// dann, wenn nichts anzuhängen ist, HEAD aber nicht mehr auf `before`
    /// steht.
    pub fn amend_commit_with_sessions(
        &self,
        before: CommitId,
        sessions: &[SessionId],
    ) -> Result<TrailerUpdate> {
        let trailers: Vec<Trailer> = sessions.iter().copied().map(Trailer::SessionId).collect();

        let message = self.message_utf8(before)?;
        let extended = Trailer::append_all(&message, &trailers);
        if extended == message {
            let current = self.head()?.commit();
            if current != Some(before) {
                return Err(GitError::ref_raced("HEAD", Some(before), current));
            }
            return Ok(TrailerUpdate::Unchanged(before));
        }

        let after = self.commit_with_message(before, &extended)?;
        self.move_head(before, after)?;

        Ok(TrailerUpdate::Amended { before, after })
    }

    /// Ob `after` aus `before` allein durch nachgerüstete Session-Trailer
    /// hervorgegangen sein kann — also genau das, was
    /// [`Repo::amend_head_with_sessions`] erzeugt.
    ///
    /// Gebraucht wird das, wo **zwei Schreiber** an denselben Commit trailern
    /// (EA-06d: erst der Witness, dann der lokale Checkpoint). Der erste
    /// Amend verschiebt HEAD; der zweite Schreiber hält noch den alten
    /// Wächter-Commit in der Hand und muss erkennen können, dass HEAD nicht
    /// weitergewandert ist, sondern nur denselben Commit mit mehr Trailern
    /// trägt.
    ///
    /// Geprüft wird streng und ohne Heuristik:
    ///
    /// - Der Kopf des Commit-Objekts (Baum, Eltern, Autor, Committer, Encoding,
    ///   Extra-Header) ist Byte für Byte gleich.
    /// - Die Message von `after` ist exakt `before` plus die Session-Trailer
    ///   von `after`, angehängt nach den Regeln von
    ///   [`Trailer::append_all`]. Jede andere Änderung an der Message — auch
    ///   ein fremder Trailer — ergibt `false`.
    ///
    /// `before == after` ergibt `true` (null nachgerüstete Trailer).
    ///
    /// Die Strenge ist gewollt einseitig: Im Zweifel `false`. Das kostet
    /// schlimmstenfalls einen fehlenden Trailer (sichtbar über `minds fsck`),
    /// nie einen Trailer am falschen Commit.
    ///
    /// # Fehler
    ///
    /// Wie beim Nachrüsten: nicht lesbare Objekte und Messages, die kein UTF-8
    /// sind ([`GitError::MessageNotUtf8`]) — ein solcher Commit wird ohnehin
    /// nie umgeschrieben.
    pub fn is_trailer_retrofit(&self, before: CommitId, after: CommitId) -> Result<bool> {
        if self.header_bytes(before)? != self.header_bytes(after)? {
            return Ok(false);
        }
        let old = self.message_utf8(before)?;
        let new = self.message_utf8(after)?;
        let trailers: Vec<Trailer> = Trailer::session_ids(&new)
            .into_iter()
            .map(Trailer::SessionId)
            .collect();
        Ok(Trailer::append_all(&old, &trailers) == new)
    }

    /// Der Kopf eines Commit-Objekts: alle Bytes vor der Leerzeile, hinter der
    /// die Message beginnt.
    fn header_bytes(&self, commit: CommitId) -> Result<Vec<u8>> {
        let object = self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?;
        let data = &object.data;
        let end = data
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .unwrap_or(data.len());
        Ok(data[..end].to_vec())
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
    /// er auf einem Branch, bewegt sich der Branch (wie bei `git commit`), und
    /// beide Reflogs bekommen eine vollständige Zeile — `HEAD@{1}` zeigt danach
    /// auf den Commit vor dem Amend. Ist HEAD detached, bewegt sich HEAD
    /// selbst. Genau das tut auch `git commit --amend` — und es ist der Grund,
    /// warum dieser Helfer im Rebase funktioniert, wo es keinen Branch gibt.
    ///
    /// # Der Lock des Branches, vorher geprüft
    ///
    /// gix-ref 0.65 läuft in eine Endlosschleife, wenn der Lock eines Refs,
    /// den die Transaktion aus HEAD **abgeleitet** hat, nicht zu bekommen ist
    /// (`file/transaction/prepare.rs`, Fehlerzweig für `LockAcquire`: Beim
    /// Suchen des Namens rückt der Cursor nie vor). Eine liegengelassene
    /// Lock-Datei, ein nicht beschreibbares `refs/heads/`, ein Pfadkonflikt —
    /// beim Witness (EA-06d) alles vom Agenten herstellbar — hielte den
    /// Schreiber für immer an. Deshalb nehmen wir den Lock des Branches vorher
    /// einmal selbst, genau wie gix ihn nähme, und geben ihn sofort wieder ab:
    /// Was ihn verhindert, ist dann ein gewöhnlicher Fehler. Welche Ketten
    /// hinter HEAD erlaubt sind und wie aufgelöst wird, steht an
    /// [`Repo::probe_branch_lock`]. Offen bleibt nur ein Lock, der genau
    /// zwischen Probe und Erwerb entsteht, bis gix den Fehler behebt.
    fn move_head(&self, before: CommitId, after: CommitId) -> Result<()> {
        self.probe_branch_lock()?;
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    // Der Reflog ist die Rückfahrkarte: Er nennt den Vorgang
                    // beim Namen und hält den alten Commit erreichbar.
                    message: "minds: session trailer retrofitted".into(),
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

    /// Nimmt die Locks aller Refs, die gix aus HEAD ableiten wird, einmal
    /// selbst und gibt sie wieder ab (siehe [`Repo::move_head`]). Detached
    /// HEAD: nichts zu prüfen — dort gibt es keinen abgeleiteten Ref.
    ///
    /// Gefolgt wird der symbolischen Kette wie bei Git höchstens
    /// [`MAX_SYMREF_DEPTH`] Stufen weit (ein Alias `master → main` bleibt
    /// möglich). Jede Stufe muss unter `refs/heads/` liegen: Nur dort liegt der
    /// Ref sicher im gemeinsamen Git-Verzeichnis, wo die Probe ihn sucht.
    /// Worktree-private Refs (`refs/worktree/*`, `refs/bisect/*`, …) legt gix
    /// anderswo ab; eine Probe am falschen Ort ließe die Endlosschleife offen.
    /// `git commit` legt einen solchen HEAD nie an — also benannt ablehnen.
    ///
    /// Gewartet wird so lange, wie gix selbst warten würde ([`LOCK_WAIT`]): Hält
    /// der andere Minds-Schreiber oder ein paralleles `git` den Lock kurz,
    /// ist das kein Fehler. Bei einer Kette aus n Gliedern sind das höchstens
    /// n × 2 s, danach wartet gix selbst noch einmal bis zu 2 s.
    fn probe_branch_lock(&self) -> Result<()> {
        let refused = |why: &str| GitError::commit("HEAD", std::io::Error::other(why.to_owned()));
        // Mit einem Ref-Namespace legt gix jeden Lock unter
        // `refs/namespaces/<ns>/…` an — die Probe säße am falschen Ort.
        if self.has_ref_namespace() {
            return Err(refused("refs namespaces are not supported"));
        }
        // Auch HEAD selbst so, wie die Transaktion es liest: nur lose, und nur
        // unter seinem eigenen Namen.
        let head = self
            .gix()
            .refs
            .try_find_packed("HEAD", None)
            .map_err(|err| GitError::commit("HEAD", err))?
            .ok_or_else(|| refused("HEAD cannot be resolved"))?;
        if head.name.as_bstr() != "HEAD" {
            return Err(refused("HEAD resolves through an ambiguous ref name"));
        }
        let gix::refs::Target::Symbolic(first) = head.target else {
            return Ok(());
        };
        let mut chain = vec![first];
        loop {
            let name = chain.last().expect("die Kette ist nie leer").clone();
            if !name.as_bstr().starts_with(b"refs/heads/") {
                return Err(refused("HEAD points at a ref outside refs/heads/"));
            }
            // Genau so auflösen wie die Transaktion von gix: nur lose Refs,
            // mit ihrer Teilnamen-Suche (`tags/…`, `heads/…`, `remotes/…`).
            // Findet die etwas unter einem anderen Namen, folgte gix einem
            // Ref, den diese Probe nie gesehen hätte.
            match self.gix().refs.try_find_packed(name.as_bstr(), None) {
                Ok(Some(found)) => {
                    if found.name != name {
                        return Err(refused("HEAD resolves through an ambiguous ref name"));
                    }
                    match found.target {
                        gix::refs::Target::Symbolic(next) => {
                            if chain.len() >= MAX_SYMREF_DEPTH {
                                return Err(refused(
                                    "HEAD points at a too long symbolic ref chain",
                                ));
                            }
                            chain.push(next);
                        }
                        gix::refs::Target::Object(_) => break,
                    }
                }
                // Nur gepackt oder ungeboren: gix legt den Lock am losen Ort
                // an — genau dort prüft die Probe unten.
                Ok(None) => break,
                Err(err) => return Err(GitError::commit("HEAD", err)),
            }
        }
        let base = self.common_dir().to_owned();
        for name in &chain {
            let path = base.join(gix::path::from_bstr(name.as_bstr()));
            gix::lock::File::acquire_to_update_resource(
                &path,
                gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_WAIT),
                Some(base.clone()),
            )
            .map(drop)
            .map_err(|err| GitError::commit("HEAD", err))?;
        }
        Ok(())
    }
}

/// Höchste Zahl symbolischer Glieder hinter HEAD — so viele, wie gix (vier
/// Runden) und Git (`SYMREF_MAXDEPTH` samt HEAD) folgen. Begrenzt zugleich, wie
/// lange die Probe auf Locks warten kann.
const MAX_SYMREF_DEPTH: usize = 4;

/// So lange wartet die Lock-Probe, so lange wartet gix danach selbst
/// (`core.filesRefLockTimeout`, festgelegt in `repo.rs`).
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

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
        // `HEAD@{1}` ist der Weg zurück, den ein Nutzer nach dem Amend nimmt:
        // Er muss auf den Commit davor zeigen.
        assert_eq!(fixture.rev_parse("HEAD@{1}"), before);
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
    fn a_retrofit_by_another_writer_is_recognized() {
        // EA-06d: Erst trailert der Witness, dann der lokale Checkpoint — der
        // zweite muss den Commit des ersten als „denselben" erkennen, auch
        // über zwei Amends und einen schon vorhandenen fremden Trailer hinweg.
        let (_fixture, repo, before) =
            repo_with_commit("fix: etwas\n\nSigned-off-by: A <a@x.invalid>\n");
        assert!(repo.is_trailer_retrofit(before, before).unwrap());

        let witness = repo.amend_head_with_sessions(&[id('a')]).unwrap().commit();
        assert!(repo.is_trailer_retrofit(before, witness).unwrap());

        let local = repo.amend_head_with_sessions(&[id('b')]).unwrap().commit();
        assert!(repo.is_trailer_retrofit(before, local).unwrap());
        assert!(repo.is_trailer_retrofit(witness, local).unwrap());
        // Die Richtung zählt: Trailer verschwinden nie.
        assert!(!repo.is_trailer_retrofit(local, witness).unwrap());
    }

    #[test]
    fn anything_but_a_session_trailer_is_not_a_retrofit() {
        let (fixture, repo, before) = repo_with_commit("feat: eins");

        // Ein neuer Commit obendrauf: andere Eltern, anderer Baum.
        fixture.write_file("b.txt", "b\n");
        let next = fixture.commit("feat: eins");
        assert!(!repo.is_trailer_retrofit(before, next).unwrap());

        // Gleicher Baum und gleiche Eltern, aber die Message wurde umformuliert
        // (`git commit --amend -m`, mit festgehaltenem Zeitstempel).
        fixture.git(&["reset", "--quiet", "--hard", &before.to_string()]);
        let reworded = amend_message(&fixture, "feat: zwei");
        assert!(!repo.is_trailer_retrofit(before, reworded).unwrap());

        // Ein fremder Trailer ist kein Session-Trailer.
        fixture.git(&["reset", "--quiet", "--hard", &before.to_string()]);
        let foreign = amend_message(&fixture, "feat: eins\n\nSigned-off-by: B <b@x.invalid>");
        assert!(!repo.is_trailer_retrofit(before, foreign).unwrap());
    }

    /// `git commit --amend` mit neuer Message. Die Fixture hält Identität und
    /// Zeitstempel fest — der Kopf des Objekts bleibt gleich, und nur die
    /// Message entscheidet.
    fn amend_message(fixture: &TempRepo, message: &str) -> CommitId {
        fixture.git(&[
            "commit",
            "--quiet",
            "--amend",
            "--allow-empty",
            "-m",
            message,
        ]);
        fixture.rev_parse("HEAD")
    }

    #[test]
    fn a_locked_branch_fails_fast_even_with_a_forever_lock_timeout() {
        // EA-06d: `.git/config` gehört dem Agenten. „Ewig warten" in der
        // Konfiguration gilt nicht, und eine liegengelassene Lock-Datei des
        // Branches endet in einem benannten Fehler statt in der Endlosschleife
        // von gix-ref (siehe `move_head`).
        let (fixture, _, before) = repo_with_commit("feat: eins");
        fixture.git(&["config", "core.filesRefLockTimeout", "-1"]);
        fixture.git(&["config", "core.packedRefsTimeout", "-1"]);
        let path = fixture.path().to_owned();
        {
            let repo = Repo::open(&path).unwrap();
            let snapshot = repo.gix().config_snapshot();
            assert_eq!(snapshot.integer("core.filesRefLockTimeout"), Some(2000));
            assert_eq!(snapshot.integer("core.packedRefsTimeout"), Some(2000));
            let discovered = Repo::discover(path.join("src")).unwrap();
            assert_eq!(
                discovered
                    .gix()
                    .config_snapshot()
                    .integer("core.filesRefLockTimeout"),
                Some(2000)
            );
        }
        std::fs::write(path.join(".git/refs/heads/main.lock"), "").unwrap();
        assert_fails_in_time(&path);
        assert_eq!(fixture.rev_parse("HEAD"), before, "HEAD unberührt");
        std::fs::remove_file(path.join(".git/refs/heads/main.lock")).unwrap();

        // Eine symbolische Kette HEAD → a → main mit gesperrtem Ende: früher
        // die Endlosschleife in gix-ref, jetzt ein benannter Fehler.
        fixture.git(&["symbolic-ref", "refs/heads/a", "refs/heads/main"]);
        fixture.git(&["symbolic-ref", "HEAD", "refs/heads/a"]);
        std::fs::write(path.join(".git/refs/heads/main.lock"), "").unwrap();
        assert_fails_in_time(&path);
        std::fs::remove_file(path.join(".git/refs/heads/main.lock")).unwrap();

        // HEAD auf einen Ref außerhalb von `refs/heads/` (worktree-privat):
        // benannt abgelehnt, ohne gix zu fragen.
        fixture.git(&["update-ref", "refs/worktree/x", &before.to_string()]);
        fixture.git(&["symbolic-ref", "HEAD", "refs/worktree/x"]);
        let err = assert_fails_in_time(&path);
        assert!(err.contains("outside refs/heads/"), "{err}");
        fixture.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);

        // Dieselbe Falle über die Präfixe, die gix auf `refs/heads/main`
        // abbildet (`main-worktree/…`, `worktrees/<n>/…`), mit gesperrtem
        // Ziel: Die Probe säße am falschen Ort — also benannt abgelehnt.
        std::fs::write(path.join(".git/refs/heads/main.lock"), "").unwrap();
        for target in [
            "main-worktree/refs/heads/main",
            "worktrees/x/refs/heads/main",
        ] {
            std::fs::write(path.join(".git/HEAD"), format!("ref: {target}\n")).unwrap();
            let err = assert_fails_in_time(&path);
            assert!(err.contains("outside refs/heads/"), "{target}: {err}");
        }
        std::fs::remove_file(path.join(".git/refs/heads/main.lock")).unwrap();
        // `git` selbst erkennt das Repo mit so einem HEAD nicht mehr.
        std::fs::write(path.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        // Ein nicht beschreibbares `refs/heads/`: Der Lock lässt sich gar nicht
        // anlegen. (Als root greifen Rechte nicht — dann nichts zu prüfen.)
        // Unix-Rechte gibt es nur dort.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let heads = path.join(".git/refs/heads");
            std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(0o555)).unwrap();
            // Scheitert eine Prüfung, muss das Verzeichnis trotzdem wieder
            // beschreibbar werden — sonst räumt `TempRepo` nicht auf.
            struct Writable(std::path::PathBuf);
            impl Drop for Writable {
                fn drop(&mut self) {
                    let _ =
                        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
                }
            }
            let restore = Writable(heads.clone());
            let probe = heads.join("probe");
            if std::fs::write(&probe, "").is_err() {
                assert_fails_in_time(&path);
            } else {
                std::fs::remove_file(&probe).unwrap();
            }
            drop(restore);
        }
        assert_eq!(fixture.rev_parse("HEAD"), before, "HEAD unberührt");

        // Ohne Hindernis geht es wieder durch — auch über einen Alias.
        fixture.git(&["symbolic-ref", "HEAD", "refs/heads/a"]);
        let repo = Repo::open(&path).unwrap();
        assert!(
            repo.amend_head_with_sessions(&[id('a')])
                .unwrap()
                .rewrote_head()
        );
        assert_ne!(
            fixture.rev_parse("refs/heads/main"),
            before,
            "der Alias bewegt main"
        );
    }

    #[test]
    fn a_briefly_held_branch_lock_is_waited_for() {
        // Zwei Schreiber (EA-06d): Hält der andere den Lock kurz, wartet die
        // Probe wie gix selbst, statt zu scheitern.
        let (fixture, repo, before) = repo_with_commit("feat: eins");
        let lock = fixture.path().join(".git/refs/heads/main.lock");
        std::fs::write(&lock, "").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::fs::remove_file(lock).unwrap();
        });
        let update = repo.amend_head_with_sessions(&[id('a')]).unwrap();
        release.join().unwrap();
        assert!(update.rewrote_head());
        assert_ne!(fixture.rev_parse("HEAD"), before);
    }

    /// Der Amend muss scheitern — und zwar binnen Frist, nicht nie. Gibt den
    /// Fehlertext zurück.
    fn assert_fails_in_time(path: &std::path::Path) -> String {
        let path = path.to_owned();
        let (done, outcome) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = Repo::open(&path)
                .and_then(|repo| repo.amend_head_with_sessions(&[id('a')]).map(drop));
            let _ = done.send(result.err().map(|err| format!("{err} {err:?}")));
        });
        outcome
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("der Amend hängt")
            .expect("das Hindernis muss den Amend scheitern lassen")
    }

    #[test]
    fn a_planted_partial_name_or_a_namespace_is_refused() {
        // Nur gepackt: Die Transaktion von gix sucht den Branch lose und fällt
        // auf Teilnamen zurück — ein gepflanztes `tags/refs/heads/main` mit
        // symbolischem Ziel lenkte sie an einen Lock, den die Probe nie sähe.
        let (fixture, _, _) = repo_with_commit("feat: eins");
        let path = fixture.path().to_owned();
        fixture.git(&["pack-refs", "--all"]);
        assert!(!path.join(".git/refs/heads/main").exists());
        std::fs::create_dir_all(path.join(".git/tags/refs/heads")).unwrap();
        std::fs::write(
            path.join(".git/tags/refs/heads/main"),
            "ref: refs/worktree/z\n",
        )
        .unwrap();
        std::fs::create_dir_all(path.join(".git/refs/worktree")).unwrap();
        std::fs::write(path.join(".git/refs/worktree/z.lock"), "").unwrap();
        let err = assert_fails_in_time(&path);
        assert!(err.contains("ambiguous ref name"), "{err}");
        std::fs::remove_dir_all(path.join(".git/tags")).unwrap();
        std::fs::remove_file(path.join(".git/refs/worktree/z.lock")).unwrap();

        // Nur gepackt, ohne Falle: geht durch.
        let repo = Repo::open(&path).unwrap();
        assert!(
            repo.amend_head_with_sessions(&[id('a')])
                .unwrap()
                .rewrote_head()
        );

        // Ein Ref-Namespace verschiebt jeden Lock-Pfad: benannt abgelehnt.
        fixture.git(&["config", "gitoxide.core.refsNamespace", "agent"]);
        assert!(Repo::open(&path).unwrap().has_ref_namespace());
        // Schon HEAD löst gix dann im Namespace auf; ob das oder die Probe
        // ablehnt — es endet benannt und binnen Frist.
        assert_fails_in_time(&path);
    }

    #[test]
    fn amending_a_commit_that_is_no_longer_head_is_a_race() {
        // EA-06d: Geprüft wurde HEAD = `first`; inzwischen steht HEAD woanders.
        // Ob noch etwas anzuhängen ist oder nicht — HEAD wird nicht angefasst.
        let (fixture, repo, first) = repo_with_commit("feat: eins");
        let trailered = repo.amend_head_with_sessions(&[id('a')]).unwrap().commit();
        fixture.write_file("b.txt", "b\n");
        let second = fixture.commit("feat: zwei");

        for sessions in [vec![id('b')], vec![]] {
            let err = repo
                .amend_commit_with_sessions(first, &sessions)
                .unwrap_err();
            assert!(matches!(err, GitError::RefRaced { .. }), "{err}");
        }
        // Auch „schon alles da" am alten Commit ist kein stiller Erfolg.
        let err = repo
            .amend_commit_with_sessions(trailered, &[id('a')])
            .unwrap_err();
        assert!(matches!(err, GitError::RefRaced { .. }), "{err}");
        assert_eq!(
            fixture.rev_parse("HEAD"),
            second,
            "HEAD bleibt unangetastet"
        );
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
