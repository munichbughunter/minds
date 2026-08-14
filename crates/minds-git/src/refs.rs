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

/// Wie lange ein Ref-Schreiber auf das Sidecar-Lock wartet, bevor er mit
/// einem benannten Fehler aufgibt (siehe `ref_write_lock`).
///
/// Die kritische Sektion ist Millisekunden kurz — wer so lange wartet, wartet
/// auf einen toten Halter (etwa nach `kill -9`), und dessen Lock-Datei nennt
/// die gix-Fehlermeldung samt Pfad.
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

    /// Wie [`commit_tree_to_ref`](Self::commit_tree_to_ref), schreibt aber
    /// **nicht**, wenn der aktuelle Blob unter `guard_path` das Prädikat
    /// `reject` erfüllt — dann kommt `Ok(None)` zurück, kein Commit.
    ///
    /// Der Guard prüft den Blob am **selben** Parent-Commit, auf den der
    /// Compare-and-Swap aufsetzt: `tree_of(parent)` liest aus einem
    /// unveränderlichen Commit, und `commit_tree_onto` sichert mit genau diesem
    /// `parent` ab. Bewegt ein paralleler Schreiber den Ref zwischen Prüfung und
    /// Commit, schlägt der CAS mit [`GitError::RefRaced`] fehl — es wird nie über
    /// den geprüften Stand hinweggeschrieben. Der Aufrufer wiederholt bei
    /// `RefRaced` und sieht dann den neuen Stand.
    ///
    /// Der Store nutzt das, damit ein Tombstone einer vergessenen Session auch
    /// unter Nebenläufigkeit nicht mit Klartext überschrieben wird
    /// (`minds-store`, Issue #6). `minds-git` selbst kennt keine Tombstones —
    /// `reject` ist ein reines Byte-Prädikat.
    pub fn commit_tree_to_ref_unless(
        &self,
        reference: &str,
        tree: TreeId,
        guard_path: &str,
        reject: impl FnOnce(&[u8]) -> bool,
        message: &str,
    ) -> Result<Option<RefUpdate>> {
        validate_minds_ref(reference)?;
        let parent = self.commit_at(reference)?;
        if let Some(parent) = parent {
            if let Some(bytes) = self.read_blob(self.tree_of(parent)?, guard_path)? {
                if reject(&bytes) {
                    return Ok(None);
                }
            }
        }
        self.commit_tree_onto(reference, tree, parent, message)
            .map(Some)
    }

    /// Liest, verändert und schreibt **eine** Datei im Baum von `reference` —
    /// atomar gegenüber parallelen Schreibern.
    ///
    /// Der springende Punkt ist derselbe wie bei
    /// [`reset_ref_to_root`](Self::reset_ref_to_root): Lesen und
    /// Compare-and-Swap teilen sich **einen** beobachteten Stand. `update`
    /// bekommt den Blob aus genau dem Commit, auf den der CAS anschließend
    /// aufsetzt. Läse ein Aufrufer erst selbst und schriebe dann über
    /// [`commit_tree_to_ref`](Self::commit_tree_to_ref), lägen zwischen seinem
    /// Lesen und dem CAS **zwei weitere** Ref-Lesungen — ein paralleler
    /// Schreiber in dieser Lücke würde nicht als Wettlauf erkannt, sondern mit
    /// veralteten Bytes überschrieben (Lost Update, minds-store Issue #4).
    ///
    /// Gibt `update` `None` zurück, wird nichts geschrieben (`Ok(None)`) — der
    /// Weg für „schon auf dem Stand". Die übrigen Dateien des Baums bleiben
    /// unangetastet, weil der neue Baum auf dem Baum desselben beobachteten
    /// Commits aufsetzt.
    ///
    /// # Fehler
    ///
    /// - [`GitError::ForbiddenRef`] — `reference` liegt außerhalb von
    ///   `refs/minds/`.
    /// - [`GitError::Identity`] — es ist keine Git-Identität konfiguriert.
    /// - [`GitError::RefRaced`] — ein paralleler Schreiber kam zuvor; der
    ///   Aufrufer wiederholt und sieht dann dessen Stand.
    pub fn update_blob_in_ref(
        &self,
        reference: &str,
        path: &str,
        message: &str,
        update: impl FnOnce(Option<&[u8]>) -> Option<Vec<u8>>,
    ) -> Result<Option<RefUpdate>> {
        validate_minds_ref(reference)?;
        let parent = self.commit_at(reference)?;
        let base = match parent {
            Some(commit) => Some(self.tree_of(commit)?),
            None => None,
        };
        let current = match base {
            Some(tree) => self.read_blob(tree, path)?,
            None => None,
        };
        let Some(bytes) = update(current.as_deref()) else {
            return Ok(None);
        };
        let blob = self.write_blob(&bytes)?;
        let tree = self.write_tree(base, [(path, blob)])?;
        self.commit_tree_onto(reference, tree, parent, message)
            .map(Some)
    }

    /// Setzt `reference` auf einen **elternlosen** Wurzel-Commit mit `tree` und
    /// kappt damit die bisherige Kette — der alte Baum ist über `<ref>~1` nicht
    /// mehr erreichbar.
    ///
    /// Anders als [`commit_tree_to_ref`](Self::commit_tree_to_ref), das den neuen
    /// Commit auf den bisherigen Stand **aufsetzt** (der alte Payload bleibt als
    /// Elter regulär erreichbar und reist bei jedem Push mit), schreibt dies einen
    /// Commit **ohne Eltern** und bewegt den Ref per Compare-and-Swap darauf. Für
    /// einen Ref, der genau eine Session hält (Store-Ref, Session-Branch), ist das
    /// ein billiger Rewrite einer privaten Orphan-Kette; für einen geteilten Baum
    /// (den Kontext-Baum) trägt der neue Wurzel-Commit den vollständigen aktuellen
    /// Baum, sodass nur die Historie wegfällt, nicht der aktuelle Stand der
    /// übrigen Sessions.
    ///
    /// Das ist der Weg, auf dem `minds forget` einen Tombstone so setzt, dass der
    /// getilgte Klartext über **keinen** Ref mehr erreichbar ist (Issue #14). Der
    /// Commit-and-Swap trennt hier bewusst zwei Dinge, die
    /// [`commit_tree_to_ref`](Self::commit_tree_to_ref) zusammenwirft: den Elter
    /// des Commits (keiner) und den erwarteten Vorzustand des Refs.
    ///
    /// # `expected` — der Vorzustand, gegen den der CAS läuft
    ///
    /// Der Aufrufer übergibt den Commit, auf den der Ref **gerade** zeigt (aus
    /// einem eigenen [`commit_at`](Self::commit_at)); `None` heißt „der Ref darf
    /// noch nicht existieren". Dass der Aufrufer ihn hereinreicht, statt dass diese
    /// Methode ihn selbst liest, ist der springende Punkt: Wer den `tree` aus dem
    /// **Baum genau dieses** Commits ableitet (der geteilte Kontext-Baum), muss die
    /// CAS an denselben Commit binden. Läse die Methode den Stand erneut, könnte
    /// sich der Ref zwischen „Basis lesen" und „CAS-Erwartung lesen" bewegen — die
    /// CAS gälte dann für den *neuen* Stand, während der `tree` auf dem *alten*
    /// gebaut wäre, und ein Lost-Update (im Kontext-Baum: eine Klartext-
    /// Auferstehung) würde festgeschrieben. Mit explizitem `expected` schlägt der
    /// CAS in diesem Fall fehl ([`GitError::RefRaced`]), der Aufrufer liest Basis
    /// und Erwartung frisch und versucht es erneut.
    ///
    /// # Fehler
    ///
    /// - [`GitError::ForbiddenRef`] — `reference` liegt außerhalb von
    ///   `refs/minds/`.
    /// - [`GitError::Identity`] — es ist keine Git-Identität konfiguriert.
    /// - [`GitError::RefRaced`] — der Ref stand nicht (mehr) auf `expected`.
    pub fn reset_ref_to_root(
        &self,
        reference: &str,
        tree: TreeId,
        expected: Option<CommitId>,
        message: &str,
    ) -> Result<RefUpdate> {
        use gix::refs::Target;
        use gix::refs::transaction::PreviousValue;

        validate_minds_ref(reference)?;

        // Zeigt der Ref (laut `expected`) schon auf einen elternlosen Commit mit
        // genau diesem Baum, ist nichts zu tun — das hält ein wiederholtes
        // `forget` (oder einen verlorenen Wettlauf, der neu aufsetzt) frei von
        // Leer-Commits. Bewusst gegen `expected` geprüft, nicht gegen den *jetzt*
        // gelesenen Stand: Ein Ref, der zwischen dem `commit_at` des Aufrufers und
        // hier weiterwandert, würde hier als `Unchanged` gemeldet statt als
        // `RefRaced`. Für die Aufrufer ist das folgenlos — die Per-Session-Refs
        // wandern nach einem Tombstone nicht mehr (ein `put` prallt ab), und der
        // geteilte Kontext-Ref trägt den Tombstone-Eintrag in jedem Kind weiter.
        if let Some(expected) = expected {
            if self.is_root_commit(expected)? && self.tree_of(expected)? == tree {
                return Ok(RefUpdate::Unchanged(expected));
            }
        }

        self.require_identity()?;

        // Erst das elternlose Commit-Objekt schreiben — noch ohne den Ref zu
        // bewegen.
        let new = self
            .gix()
            .new_commit(message, tree.to_gix(), None::<gix::ObjectId>)
            .map_err(|err| GitError::commit(reference, err))?
            .id;

        // Dann den Ref per Compare-and-Swap darauf setzen: existiert er, muss er
        // noch auf `expected` zeigen; ist `expected` None, darf ihn niemand
        // zwischenzeitlich angelegt haben. Verify-under-lock wie in
        // [`commit_tree_onto`](Self::commit_tree_onto): Der Vergleich zählt
        // erst mit gehaltenem Lock (siehe [`ref_write_lock`](Self::ref_write_lock)),
        // der gix-eigene `PreviousValue` bleibt als zweites Netz bestehen.
        let _serialized = self.ref_write_lock()?;
        let now = self.commit_at(reference)?;
        if now != expected {
            return Err(GitError::ref_raced(reference, expected, now));
        }

        let constraint = match expected {
            Some(expected) => PreviousValue::MustExistAndMatch(Target::Object(expected.to_gix())),
            None => PreviousValue::MustNotExist,
        };

        match self.gix().reference(reference, new, constraint, message) {
            Ok(_) => Ok(match expected {
                Some(_) => RefUpdate::Advanced(CommitId::from_gix(new)),
                None => RefUpdate::Created(CommitId::from_gix(new)),
            }),
            Err(err) => Err(GitError::commit(reference, err)),
        }
    }

    /// Löscht `reference`, falls vorhanden — ein nicht existierender Ref ist kein
    /// Fehler (idempotent).
    ///
    /// `minds forget` braucht das für **verwaiste** Tracking-Refs
    /// (`refs/minds/remotes/…`), deren maßgeblicher Ref gar nicht mehr existiert.
    /// Zeigt der maßgebliche Ref dagegen auf einen Tombstone, wird der Tracking-
    /// Ref nicht gelöscht, sondern mit [`set_ref`](Self::set_ref) darauf umgesetzt
    /// (siehe dort). Läuft **bewusst ohne** das Sidecar-Lock aus
    /// `ref_write_lock` und verlässt sich auf gix' Erwartungswert — der ist
    /// unter Nebenläufigkeit nicht verlässlich (#4), aber die Tracking-
    /// Buchhaltung ist jederzeit aus dem maßgeblichen Stand neu bestimmbar:
    /// Ein verlorener Wettlauf kostet hier schlimmstenfalls einen erneuten
    /// Push, nie Inhalt.
    pub fn delete_ref(&self, reference: &str) -> Result<()> {
        validate_minds_ref(reference)?;
        match self
            .gix()
            .try_find_reference(reference)
            .map_err(|err| GitError::reference(reference, err))?
        {
            Some(found) => found
                .delete()
                .map_err(|err| GitError::reference(reference, err)),
            None => Ok(()),
        }
    }

    /// Setzt `reference` auf `target` — **ohne** Fast-Forward-Bedingung, legt ihn
    /// bei Bedarf an.
    ///
    /// Nur für die eigene Push-Buchhaltung (`refs/minds/remotes/…`), die den
    /// zuletzt bekannten Remote-Stand spiegelt und jederzeit neu bestimmbar ist —
    /// deshalb ist ein force-Set hier vertretbar, wo er es für einen maßgeblichen
    /// Ref nie wäre. `minds forget` setzt damit einen Tracking-Ref, der auf einen
    /// Klartext-Commit zeigt, auf den elternlosen Tombstone um: Der Klartext ist
    /// danach über ihn nicht mehr erreichbar (gc-Ziel), und `minds sync` sieht
    /// „schon auf Stand" statt einen non-fast-forward-Push zu versuchen (#14).
    pub fn set_ref(&self, reference: &str, target: CommitId) -> Result<()> {
        use gix::refs::transaction::PreviousValue;
        validate_minds_ref(reference)?;
        self.gix()
            .reference(
                reference,
                target.to_gix(),
                PreviousValue::Any,
                "minds: Tracking-Ref auf Tombstone umgesetzt",
            )
            .map(|_| ())
            .map_err(|err| GitError::reference(reference, err))
    }

    /// Ob `commit` ein Wurzel-Commit ist — ohne Eltern.
    fn is_root_commit(&self, commit: CommitId) -> Result<bool> {
        Ok(self
            .gix()
            .find_commit(commit.to_gix())
            .map_err(|err| GitError::read_object(commit, err))?
            .parent_ids()
            .next()
            .is_none())
    }

    /// Serialisiert alle Minds-Ref-Schreiber dieses Repositories — prozessweit
    /// **und** prozessübergreifend (Datei-Lock im **geteilten**
    /// Git-Verzeichnis, damit auch verlinkte Worktrees dasselbe Lock nehmen).
    ///
    /// Warum es das braucht: gix (0.85) verifiziert den `PreviousValue` einer
    /// Ref-Transaktion gegen einen Stand, der **vor** dem Lock-Erwerb gelesen
    /// wurde. Zwei Schreiber, die beide denselben alten Stand sahen, bekommen
    /// dann **beide** `Ok` — der zweite hängt seine Kette am ersten vorbei
    /// wieder an den alten Commit, und dessen Schreiben ist still verloren
    /// (Lost Update, minds-store #4; im Test `concurrent_links_lose_no_edge`
    /// reproduzierbar). Unter diesem Lock wird der Stand deshalb **selbst**
    /// frisch gelesen und verglichen, bevor gix schreibt: Staleness wird zu
    /// [`GitError::RefRaced`], nie zu einem stillen Überschreiben.
    ///
    /// Ein grobes, blockierendes Lock ist hier die richtige Währung: Die
    /// Schreibvorgänge sind Millisekunden kurz, und `git` selbst fasst
    /// `refs/minds/*` nie an.
    ///
    /// **Bewusst ausgenommen:** [`set_ref`](Self::set_ref) und
    /// [`delete_ref`](Self::delete_ref) — sie pflegen ausschließlich die
    /// Tracking-Buchhaltung (`refs/minds/remotes/…`), die jederzeit aus dem
    /// maßgeblichen Stand neu bestimmbar ist; ein verlorener Wettlauf kostet
    /// dort schlimmstenfalls einen erneuten Push, nie Inhalt.
    fn ref_write_lock(&self) -> Result<gix::lock::Marker> {
        let dir = self.common_dir().join("minds");
        std::fs::create_dir_all(&dir)
            .map_err(|err| GitError::commit("refs/minds (Lock-Verzeichnis)", err))?;
        // Blockierend mit Backoff: Die kritische Sektion ist Millisekunden
        // kurz; wer nach LOCK_TIMEOUT noch wartet, wartet auf einen toten
        // Halter — dann ist ein benannter Fehler richtiger als ewiges Warten.
        gix::lock::Marker::acquire_to_hold_resource(
            dir.join("refs-write"),
            gix::lock::acquire::Fail::AfterDurationWithBackoff(LOCK_TIMEOUT),
            None,
        )
        .map_err(|_| GitError::LockUnavailable {
            path: dir.join("refs-write.lock"),
        })
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
        use gix::refs::Target;
        use gix::refs::transaction::PreviousValue;

        // Schon auf dem Stand? Dann kein Commit. Das hält die Kontext-Historie
        // frei von Rauschen und macht ein wiederholtes `put` gratis.
        if let Some(parent) = parent {
            if self.tree_of(parent)? == tree {
                return Ok(RefUpdate::Unchanged(parent));
            }
        }

        self.require_identity()?;

        // Erst das Commit-Objekt schreiben — noch ohne den Ref zu bewegen. Der
        // Ref-Wechsel läuft dann als **expliziter** Compare-and-Swap, wie in
        // [`reset_ref_to_root`](Self::reset_ref_to_root): gix' `commit`-
        // Komfortweg hat den Erwartungswert unter echter Nebenläufigkeit nicht
        // zuverlässig durchgesetzt — zwei parallele Schreiber bekamen beide
        // `Ok`, und der zweite hängte seine Kette am ersten vorbei wieder an
        // den alten Stand (Lost Update, minds-store #4).
        let new = self
            .gix()
            .new_commit(message, tree.to_gix(), parent.map(CommitId::to_gix))
            .map_err(|err| GitError::commit(reference, err))?
            .id;

        // Verify-under-lock: Erst mit gehaltenem Lock zählt der Vergleich —
        // siehe [`ref_write_lock`](Self::ref_write_lock). Der gix-eigene
        // `PreviousValue` bleibt als zweites Netz bestehen.
        let _serialized = self.ref_write_lock()?;
        let current = self.commit_at(reference)?;
        if current != parent {
            return Err(GitError::ref_raced(reference, parent, current));
        }

        let constraint = match parent {
            Some(parent) => PreviousValue::MustExistAndMatch(Target::Object(parent.to_gix())),
            None => PreviousValue::MustNotExist,
        };
        match self.gix().reference(reference, new, constraint, message) {
            Ok(_) => Ok(match parent {
                Some(_) => RefUpdate::Advanced(CommitId::from_gix(new)),
                None => RefUpdate::Created(CommitId::from_gix(new)),
            }),
            Err(err) => Err(GitError::commit(reference, err)),
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

    // --- update_blob_in_ref: atomares Read-Modify-Write (#4) ----------------

    #[test]
    fn update_blob_reads_and_writes_the_same_observed_state() {
        let (_fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/links";

        repo.update_blob_in_ref(reference, "links.json", "eins", |current| {
            assert!(current.is_none());
            Some(b"[1]".to_vec())
        })
        .unwrap()
        .expect("erster Stand wird geschrieben");

        let update = repo
            .update_blob_in_ref(reference, "links.json", "zwei", |current| {
                assert_eq!(current, Some(&b"[1]"[..]));
                Some(b"[1,2]".to_vec())
            })
            .unwrap();
        assert!(update.is_some());
        assert_eq!(
            repo.read_blob_at(reference, "links.json").unwrap().unwrap(),
            b"[1,2]"
        );
    }

    #[test]
    fn update_blob_leaves_neighbours_standing() {
        // Der neue Baum setzt auf dem beobachteten auf — Nachbarn überleben.
        let (_fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/session";
        let payload = repo.write_blob(b"payload").unwrap();
        let tree = repo.write_tree(None, [("session.json", payload)]).unwrap();
        repo.commit_tree_to_ref(reference, tree, "payload").unwrap();

        repo.update_blob_in_ref(reference, "links.json", "kante", |_| Some(b"[]".to_vec()))
            .unwrap();

        assert_eq!(
            repo.read_blob_at(reference, "session.json")
                .unwrap()
                .unwrap(),
            b"payload"
        );
    }

    #[test]
    fn a_none_from_update_writes_nothing() {
        let (_fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/noop";
        let update = repo
            .update_blob_in_ref(reference, "links.json", "nichts", |_| None)
            .unwrap();
        assert!(update.is_none());
        assert_eq!(repo.commit_at(reference).unwrap(), None);
    }

    #[test]
    fn a_stale_expectation_is_rejected_across_handles() {
        // Sonde: Zwei Handles, sequenziell. Ein CAS mit veralteter Erwartung
        // muss scheitern — sonst ist der Erwartungswert Fiktion.
        let (fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/probe";
        repo.update_blob_in_ref(reference, "f", "a", |_| Some(b"a".to_vec()))
            .unwrap();
        let stale = repo.commit_at(reference).unwrap();

        let other = Repo::open(fixture.path()).unwrap();
        other
            .update_blob_in_ref(reference, "f", "b", |_| Some(b"b".to_vec()))
            .unwrap();

        let blob = repo.write_blob(b"c").unwrap();
        let tree = repo.write_tree(None, [("f", blob)]).unwrap();
        let outcome = repo.commit_tree_onto(reference, tree, stale, "stale");
        assert!(
            matches!(outcome, Err(GitError::RefRaced { .. })),
            "{outcome:?}"
        );
        // Der Stand des Gewinners steht unverändert.
        assert_eq!(
            repo.read_blob_at(reference, "f").unwrap().unwrap(),
            b"b",
            "der veraltete CAS hat durchgeschrieben"
        );
    }

    #[test]
    fn threaded_updates_lose_nothing() {
        // Wie der deterministische Wettlauf-Test, aber mit echten Threads:
        // Jeder Marker, dessen Update `Ok` meldet, muss im Endstand stehen.
        let (fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/threads";
        repo.update_blob_in_ref(reference, "l.json", "init", |_| Some(b"[]".to_vec()))
            .unwrap();

        let path = fixture.path().to_path_buf();
        let handles: Vec<_> = (0..2)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let repo = Repo::open(&path).unwrap();
                    for i in 0..3 {
                        let marker = format!("\"m{writer}{i}\"");
                        let mut tries = 0;
                        loop {
                            tries += 1;
                            let outcome =
                                repo.update_blob_in_ref(reference, "l.json", "m", |current| {
                                    let current = std::str::from_utf8(current.unwrap())
                                        .unwrap()
                                        .trim_end_matches(']')
                                        .to_string();
                                    let sep = if current.ends_with('[') { "" } else { "," };
                                    Some(format!("{current}{sep}{marker}]").into_bytes())
                                });
                            match outcome {
                                Ok(_) => break,
                                Err(GitError::RefRaced { .. }) if tries < 10 => continue,
                                Err(err) => panic!("w{writer} i{i}: {err}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = repo.read_blob_at(reference, "l.json").unwrap().unwrap();
        let content = String::from_utf8(content).unwrap();
        for writer in 0..2 {
            for i in 0..3 {
                assert!(
                    content.contains(&format!("\"m{writer}{i}\"")),
                    "m{writer}{i} fehlt: {content}"
                );
            }
        }
    }

    #[test]
    fn linked_worktrees_share_the_write_lock() {
        // Der Blocker aus dem Review: `git_dir()` ist im verlinkten Worktree
        // privat, die Refs sind geteilt — das Lock muss aus dem Common Dir
        // kommen, sonst serialisiert es nur je Worktree.
        let (fixture, repo, _tree) = repo_with_pending_tree();
        fixture.git(&["worktree", "add", "--detach", "wt"]);
        let linked = Repo::open(fixture.path().join("wt")).unwrap();

        assert_eq!(
            std::fs::canonicalize(repo.common_dir()).unwrap(),
            std::fs::canonicalize(linked.common_dir()).unwrap(),
            "beide Worktrees müssen denselben Lock-Ort sehen"
        );

        // Und der Wettlauf über die Worktree-Grenze wird erkannt, nicht
        // überschrieben — dieselbe Sonde wie oben, nur mit dem verlinkten
        // Worktree als Gewinner.
        let reference = "refs/minds/test/worktree";
        repo.update_blob_in_ref(reference, "f", "a", |_| Some(b"a".to_vec()))
            .unwrap();
        let err = repo
            .update_blob_in_ref(reference, "f", "verlierer", |current| {
                assert_eq!(current, Some(&b"a"[..]));
                linked
                    .update_blob_in_ref(reference, "f", "gewinner", |_| Some(b"b".to_vec()))
                    .unwrap();
                Some(b"c".to_vec())
            })
            .unwrap_err();
        assert!(matches!(err, GitError::RefRaced { .. }), "{err:?}");
        assert_eq!(repo.read_blob_at(reference, "f").unwrap().unwrap(), b"b");
    }

    #[test]
    fn a_leftover_lock_names_itself() {
        // Stirbt ein Halter hart (SIGKILL), bleibt die Lock-Datei liegen. Der
        // Fehler muss dann den Pfad nennen, damit die Abhilfe („von Hand
        // löschen") ohne Code-Kenntnis lesbar ist. Der Test wartet bewusst
        // die Backoff-Frist ab.
        let (_fixture, repo, _tree) = repo_with_pending_tree();
        let dir = repo.common_dir().join("minds");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("refs-write.lock"), b"").unwrap();

        let err = repo
            .update_blob_in_ref("refs/minds/test/lock", "f", "x", |_| Some(b"x".to_vec()))
            .unwrap_err();
        assert!(err.to_string().contains("refs-write.lock"), "{err}");
        assert!(err.to_string().contains("von Hand"), "{err}");
    }

    #[test]
    fn a_racing_writer_is_detected_not_overwritten() {
        // Der Wettlauf aus minds-store #4, deterministisch: Während `update`
        // noch mergt, schreibt ein paralleler Schreiber denselben Ref. Der CAS
        // muss das als `RefRaced` melden — nicht mit veralteten Bytes gewinnen.
        let (fixture, repo, _tree) = repo_with_pending_tree();
        let reference = "refs/minds/test/raced";

        repo.update_blob_in_ref(reference, "links.json", "basis", |_| {
            Some(b"[\"a\"]".to_vec())
        })
        .unwrap();

        let racer = Repo::open(fixture.path()).unwrap();
        let err = repo
            .update_blob_in_ref(reference, "links.json", "verlierer", |current| {
                assert_eq!(current, Some(&b"[\"a\"]"[..]));
                // Der Gewinner schreibt, während wir noch auf dem alten Stand
                // arbeiten.
                racer
                    .update_blob_in_ref(reference, "links.json", "gewinner", |_| {
                        Some(b"[\"a\",\"b\"]".to_vec())
                    })
                    .unwrap();
                Some(b"[\"a\",\"c\"]".to_vec())
            })
            .unwrap_err();
        assert!(matches!(err, GitError::RefRaced { .. }), "{err}");

        // Der Stand des Gewinners steht — nichts wurde überschrieben.
        assert_eq!(
            repo.read_blob_at(reference, "links.json").unwrap().unwrap(),
            b"[\"a\",\"b\"]"
        );
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

    #[test]
    fn the_guard_refuses_to_write_over_a_matching_parent() {
        // Das Herz des Reanimations-Schutzes (#6): Trägt der Parent unter dem
        // Guard-Pfad einen Inhalt, den das Prädikat ablehnt, wird nicht
        // geschrieben — `Ok(None)` — und der Ref bleibt, wo er ist.
        let (fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();

        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let outcome = repo
            .commit_tree_to_ref_unless(
                DEFAULT_CONTEXT_REF,
                tree2,
                "sessions/b3/aa.json",
                |bytes| bytes == b"{\"session\":\"aa\"}",
                "minds: 2",
            )
            .unwrap();

        assert!(outcome.is_none(), "der Guard hätte ablehnen müssen");
        // Der Ref steht unverändert auf dem ersten Commit — echtes git bestätigt.
        assert_eq!(
            repo.commit_at(DEFAULT_CONTEXT_REF).unwrap(),
            Some(first.commit())
        );
        assert_eq!(
            fixture.hash(DEFAULT_CONTEXT_REF),
            first.commit().to_string()
        );
    }

    #[test]
    fn the_guard_writes_when_the_parent_passes_or_is_absent() {
        let (_fixture, repo, tree) = repo_with_pending_tree();

        // Kein Parent: Der Guard kann nichts prüfen — geschrieben wird trotzdem,
        // selbst wenn das Prädikat alles ablehnen würde.
        let created = repo
            .commit_tree_to_ref_unless(
                DEFAULT_CONTEXT_REF,
                tree,
                "sessions/b3/aa.json",
                |_| true,
                "minds: 1",
            )
            .unwrap()
            .expect("ohne Parent gibt es nichts abzulehnen");
        assert!(matches!(created, RefUpdate::Created(_)));

        // Parent vorhanden, aber sein Blob erfüllt das Prädikat nicht: Es wird
        // regulär aufgesetzt.
        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let advanced = repo
            .commit_tree_to_ref_unless(
                DEFAULT_CONTEXT_REF,
                tree2,
                "sessions/b3/aa.json",
                |bytes| bytes == b"ein Inhalt, den es hier nicht gibt",
                "minds: 2",
            )
            .unwrap()
            .expect("der Guard lässt den nicht passenden Parent durch");
        assert!(matches!(advanced, RefUpdate::Advanced(_)));

        // Parent vorhanden, aber der Guard-Pfad fehlt in seinem Baum: `read_blob`
        // liefert `None`, das Prädikat wird nie gefragt — geschrieben wird.
        let blob = repo.write_blob(b"{\"session\":\"cc\"}").unwrap();
        let tree3 = repo
            .write_tree(Some(tree2), [("sessions/b3/cc.json", blob)])
            .unwrap();
        let advanced = repo
            .commit_tree_to_ref_unless(
                DEFAULT_CONTEXT_REF,
                tree3,
                "sessions/b3/gibt-es-nicht.json",
                |_| true,
                "minds: 3",
            )
            .unwrap()
            .expect("fehlt der Guard-Pfad, greift der Guard nicht");
        assert!(matches!(advanced, RefUpdate::Advanced(_)));
    }

    #[test]
    fn reset_ref_to_root_writes_a_parentless_commit_and_cuts_the_history() {
        // Der Kern von #14: Nach dem Reset ist der alte Baum über `<ref>~1` nicht
        // mehr erreichbar, weil der neue Commit keinen Elter hat.
        let (fixture, repo, tree) = repo_with_pending_tree();
        repo.commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree, "minds: 1")
            .unwrap();
        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let tree2 = repo
            .write_tree(Some(tree), [("sessions/b3/bb.json", blob)])
            .unwrap();
        let beeltert = repo
            .commit_tree_to_ref(DEFAULT_CONTEXT_REF, tree2, "minds: 2")
            .unwrap();
        // Vorbedingung: Es gibt eine Historie — `<ref>~1` löst auf.
        let parent_before = fixture.git(&["rev-parse", &format!("{DEFAULT_CONTEXT_REF}~1")]);
        assert!(!parent_before.trim().is_empty());

        let tomb = repo.write_blob(b"tombstone").unwrap();
        let tomb_tree = repo
            .write_tree(None, [("sessions/b3/aa.json", tomb)])
            .unwrap();
        let expected = repo.commit_at(DEFAULT_CONTEXT_REF).unwrap();
        let update = repo
            .reset_ref_to_root(DEFAULT_CONTEXT_REF, tomb_tree, expected, "minds: vergessen")
            .unwrap();

        assert!(matches!(update, RefUpdate::Advanced(_)));
        assert_ne!(update.commit(), beeltert.commit(), "ein neuer Commit");
        // Der neue Commit ist elternlos.
        let parents = fixture.git(&["log", "-1", "--format=%P", DEFAULT_CONTEXT_REF]);
        assert_eq!(
            parents.trim(),
            "",
            "der Tombstone-Commit muss elternlos sein"
        );
        // Und `<ref>~1` löst nicht mehr auf — die Kette ist gekappt.
        let has_parent = std::process::Command::new("git")
            .arg("-C")
            .arg(fixture.path())
            .args(["rev-parse", "--verify", &format!("{DEFAULT_CONTEXT_REF}~1")])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!has_parent, "der Ref darf keinen Eltern-Commit mehr haben");
    }

    #[test]
    fn reset_ref_to_root_creates_the_ref_when_absent() {
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let update = repo
            .reset_ref_to_root(DEFAULT_CONTEXT_REF, tree, None, "minds: frisch")
            .unwrap();
        assert!(matches!(update, RefUpdate::Created(_)));
    }

    #[test]
    fn reset_ref_to_root_is_idempotent_on_an_orphan_with_the_same_tree() {
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let first = repo
            .reset_ref_to_root(DEFAULT_CONTEXT_REF, tree, None, "minds: vergessen")
            .unwrap();
        let again = repo
            .reset_ref_to_root(
                DEFAULT_CONTEXT_REF,
                tree,
                Some(first.commit()),
                "minds: vergessen",
            )
            .unwrap();
        assert_eq!(again, RefUpdate::Unchanged(first.commit()));
    }

    #[test]
    fn reset_ref_to_root_reports_a_race_when_the_ref_moved_off_expected() {
        // B2-Absicherung (#14): Ein `expected`, das nicht mehr stimmt, führt zu
        // `RefRaced` — nicht zu einem Commit auf veralteter Basis.
        let (_fixture, repo, tree) = repo_with_pending_tree();
        let real = repo
            .reset_ref_to_root(DEFAULT_CONTEXT_REF, tree, None, "minds: 1")
            .unwrap();

        // Ein anderer Baum, aber mit einem *veralteten* Erwartungswert (None —
        // „Ref existiert nicht", obwohl er auf `real` steht).
        let blob = repo.write_blob(b"{\"session\":\"bb\"}").unwrap();
        let other = repo
            .write_tree(None, [("sessions/b3/bb.json", blob)])
            .unwrap();
        let raced = repo.reset_ref_to_root(DEFAULT_CONTEXT_REF, other, None, "minds: 2");
        assert!(
            matches!(raced, Err(GitError::RefRaced { .. })),
            "erwartete RefRaced, war {raced:?}"
        );
        // Der Ref steht unverändert auf dem ersten Reset.
        assert_eq!(
            repo.commit_at(DEFAULT_CONTEXT_REF).unwrap(),
            Some(real.commit())
        );
    }
}
