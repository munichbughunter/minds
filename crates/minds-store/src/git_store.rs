//! Die gemeinsame Implementierung beider Backends.
//!
//! [`GitStore`] ist ein Kontext-Store in **einem** Git-Repository unter **einem**
//! Ref. Welches Repository das ist, weiß er nicht und muss er nicht wissen —
//! genau darin besteht der Unterschied zwischen In-Repo und Child-Repo, und in
//! nichts sonst (Plan: „identischer Baum, nur in einem separaten Repo").
//!
//! Deshalb ist dieser Typ privat und die beiden öffentlichen Backends
//! ([`InRepoStore`](crate::InRepoStore), [`ChildRepoStore`](crate::ChildRepoStore))
//! sind Hüllen darum. Der Nutzen ist keiner der Bequemlichkeit, sondern der
//! Nachweisbarkeit: Es gibt **eine** Stelle, die Pfade baut und Refs bewegt.
//! Zwei Implementierungen könnten auseinanderlaufen; diese eine kann es nicht,
//! und `both_backends_write_the_same_tree` in `child_repo.rs` weist es nach.
//!
//! # Der Schreibweg
//!
//! ```text
//! write_blob(bytes) → write_tree(bisheriger Baum + Pfad) → commit_tree_to_ref
//! ```
//!
//! Genau die Reihenfolge, die Git vorgibt: erst Inhalt, dann Baum, dann Commit,
//! dann Ref.
//!
//! # Dedup hat zwei Schichten, und nur die untere ist die Garantie
//!
//! **Unten steht Git.** Gleiche Bytes ergeben denselben Blob-Hash, gleicher Blob
//! unter gleichem Pfad denselben Baum-Hash, und `commit_tree_to_ref` schreibt
//! keinen Commit, wenn der Ref schon auf diesen Baum zeigt. Ein zweites `put`
//! derselben Session bleibt damit folgenlos, **auch wenn es niemand kommen
//! sieht** — zwei parallele Läufe, die beide „ist noch nicht da" feststellen,
//! landen trotzdem bei einem Eintrag. [`Put::AlreadyPresent`] ist dieses
//! `RefUpdate::Unchanged`, eine Ebene höher übersetzt.
//!
//! **Oben steht ein Vergleich.** [`GitStore::put_bytes`] sieht zuerst nach, ob
//! unter dem Pfad schon genau diese Bytes liegen, und hört dann sofort auf. Das
//! ist eine Abkürzung, keine zweite Zusage: Sie spart den Baum, und der wächst
//! mit dem Store. Ohne sie kostet ein wiederholtes `put` bei N Sessions einen
//! neu geschriebenen Baum mit N Einträgen — folgenlos, aber nicht gratis, und
//! ein `post-commit`-Hook wiederholt gern.
//!
//! Verglichen werden **Bytes, nicht bloß die Existenz**. Der Unterschied fällt
//! nur in einem Fall auf, aber in dem zählt er: Liegt unter dem Session-Pfad
//! etwas Fremdes (jemand hat den Blob ersetzt), meldete eine reine
//! Existenzprüfung „liegt schon da" und ließe den Store kaputt. Der
//! Byte-Vergleich fällt durch, der reguläre Weg läuft, und der richtige Inhalt
//! steht wieder da. Repariert wird dabei sichtbar — es entsteht ein Commit.
//!
//! # Wettläufe werden wiederholt, nicht gemeldet
//!
//! Ein `post-commit`-Hook und ein Aufruf von Hand reichen für zwei gleichzeitige
//! `minds capture`. `commit_tree_to_ref` erkennt das (Compare-and-Swap) und
//! bricht mit `RefRaced` ab, statt die fremde Session zu überschreiben. Der
//! richtige Umgang damit ist neu aufzusetzen und es noch einmal zu versuchen —
//! und das gehört hierher und nicht in die CLI: Es ist nichts zu entscheiden,
//! nur zu wiederholen. Nach [`PUT_ATTEMPTS`] Versuchen gibt der Store auf; wer
//! dann noch verliert, hat kein Wettlauf-Problem, sondern eines mit dem Ref.
//!
//! # Ein fehlender Ref ist ein leerer Store
//!
//! Vor dem ersten `minds capture` gibt es `refs/minds/context` nicht. `list`
//! liefert dann eine leere Liste, `get` ein `None` — dieselbe Linie wie in
//! `minds-git`: Was regulär vorkommt, ist kein Fehler.
//!
//! # `exists` bleibt beim Default
//!
//! Der Trait-Default liest den Blob und wirft ihn weg. Billiger ginge es mit
//! einem reinen Baum-Lookup, aber `minds-git` bietet ihn heute nicht an, und
//! eine Session ist ein paar Kilobyte. Kommt die Abkürzung, ist sie eine
//! Ergänzung dort und ein Vierzeiler hier.

use std::collections::BTreeSet;

use minds_core::{Evidence, SESSION_ID_PREFIX, SessionId};
use minds_git::{GitError, RefUpdate, Repo};

use crate::bytes::SessionBytes;
use crate::error::{Result, StoreError};
use crate::index::CommitIndex;
use crate::layout::{id_of_path, path_of};
use crate::store::{ContextStore, Forget, ForgottenPlace, Put};

/// Wie oft [`GitStore::put_bytes`] einen verlorenen Wettlauf am Ref wiederholt,
/// bevor er ihn meldet.
const PUT_ATTEMPTS: u32 = 3;

/// Der einzige Nachbar der Sessions im Baum — die Commit→Session-Zuordnung, die
/// nicht in die Commit-Message passt (siehe [`crate::index`]).
const INDEX_PATH: &str = "index.json";

/// Der Namensraum, in dem die **Nutzlast** einer Session liegt: ein Ref je
/// Session, benannt nach ihrem vollen Hash.
///
/// # Warum nicht mehr alles unter einem Ref
///
/// Bis v0.2 lag jede Session als Blob im Baum von `refs/minds/context`. Das
/// machte diesen einen Ref zum **Serialisierungspunkt**: Jeder Checkpoint schrieb
/// den Baum mit *allen* N Einträgen neu und bewegte den Ref, und zwei Agents, die
/// gleichzeitig eincheckten, rannten in einen Compare-and-Swap gegeneinander.
/// Beim Push traf sich dieselbe Enge noch einmal — ein Ref, den alle bewegen,
/// divergiert zwischen zwei Maschinen und muss zusammengeführt werden.
///
/// Ein Ref je Session löst alle drei Engen auf einmal:
///
/// - **Schreiben ist O(1).** Der Baum hat einen Eintrag, egal wie viele Sessions
///   der Store hält.
/// - **Kein Wettlauf.** Der Ref-Name *ist* der Inhalts-Hash; zwei Agents, die
///   verschiedene Sessions schreiben, fassen verschiedene Refs an. Wer dieselbe
///   Session schreibt, schreibt denselben Baum — der zweite Lauf ist ein No-op.
/// - **Kein divergenter Push.** Ein Session-Ref entsteht genau einmal und ändert
///   sich danach nicht mehr (außer beim Vergessen, und das ist ein
///   Fast-Forward). Er kann nicht non-fast-forward abprallen.
///
/// `refs/minds/store/` und nicht `refs/minds/sessions/`: Letzteres trägt die
/// *browsbaren* Branches des Child-Backends (gekürzter Hash, mit `session.md`
/// zum Anschauen). Die Nutzlast und die Ansicht sind zwei Dinge; sie sollen
/// nicht im selben Namensraum liegen.
const SESSION_STORE_PREFIX: &str = "refs/minds/store/";

/// Die Nutzlast im Baum eines Session-Refs.
const SESSION_FILE: &str = "session.json";

/// Die Kanten `commit → diese Session` im Baum eines Session-Refs.
///
/// Der Commit-Index lag bis v0.2 als **eine** `index.json` im Kontext-Baum. Das
/// war der letzte gemeinsame Schreibvorgang des heißen Pfades: Jeder Checkpoint
/// las ihn, ergänzte eine Zeile und schrieb ihn ganz zurück — zwei Agents
/// gleichzeitig ergaben einen Wettlauf, zwei Maschinen einen divergenten Push.
///
/// Hier steht nur der Anteil *dieser* Session. Der Gesamtindex ist die
/// Vereinigung über alle Session-Refs; er wird gelesen, nie geschrieben. Der
/// Tausch ist bewusst: Der heiße Pfad (Checkpoint) wird O(1) und
/// konfliktfrei, die kalten Pfade (`fsck`, `render`, `show`) lesen dafür N
/// kleine Blobs statt einen großen.
const SESSION_LINKS_FILE: &str = "links.json";

/// Die Dateien im Baum eines Session-Branches: die Session als JSON (die
/// maßgebliche, content-adressierte Form) und als `session.md` (die Forge
/// rendert sie nativ — der Branch wird zur lesbaren Seite, Track C). Ein Branch
/// trägt genau eine Session, also brauchen sie keinen Ordner.
const SESSION_BRANCH_FILE: &str = "session.json";
const SESSION_BRANCH_MD: &str = "session.md";

/// Wie viele Hex-Zeichen der SessionId in den Branch-Namen wandern. 16 (64 Bit)
/// halten den Namen kurz und sind gegen Kollisionen mehr als genug — ein
/// Zusammenstoß bräuchte Milliarden Sessions, und der volle Hash steht ohnehin
/// in der `session.json` des Branches. Ein (praktisch unmöglicher) Kollisions-
/// fall überschriebe nur einen Browsing-Branch; der content-adressierte Store
/// bleibt die maßgebliche Quelle und ist unberührt.
const SESSION_BRANCH_HEX: usize = 16;

/// Ein Kontext-Store in einem Git-Repository, unter einem Ref.
///
/// Hält ein eigenes [`Repo`]-Handle, statt eines zu leihen: Sonst trüge jeder,
/// der einen Store hält, dessen Lebensdauer mit — bis hinauf zum
/// `Box<dyn ContextStore>` der CLI, das dann `Box<dyn ContextStore + '_>` hieße.
/// Ein zweites Handle auf dasselbe Repository ist billig.
#[derive(Debug)]
pub(crate) struct GitStore {
    repo: Repo,
    reference: String,
}

impl GitStore {
    /// Ein Store auf `repo` unter `reference`.
    pub(crate) fn new(repo: Repo, reference: impl Into<String>) -> Self {
        Self {
            repo,
            reference: reference.into(),
        }
    }

    /// Legt den Ref fest, unter dem der Store liegt.
    ///
    /// Nicht geprüft, und das mit Absicht: Die Regel „Minds schreibt nur
    /// unterhalb von `refs/minds/`" steht in `minds-git` und wird dort beim
    /// Schreiben durchgesetzt. Sie hier zu wiederholen hieße, sie an zwei
    /// Stellen zu pflegen. Ein Store auf einem fremden Ref kann lesen und
    /// scheitert beim ersten `put`.
    pub(crate) fn with_ref(self, reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            ..self
        }
    }

    /// Das Repository, in dem der **Kontext** liegt.
    pub(crate) fn context_repo(&self) -> &Repo {
        &self.repo
    }

    /// Der Ref, unter dem dieser Store liegt.
    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    /// Ein Schreibversuch für die Nutzlast einer Session: Blob, Baum, Commit,
    /// **eigener** Ref.
    ///
    /// Der Baum trägt genau eine Datei und setzt auf **nichts** auf
    /// (`write_tree(None, …)`) — daher ist der Schreibvorgang unabhängig davon,
    /// wie viele Sessions der Store schon hält. Genau das war der Unterschied,
    /// um den es beim Umzug ging.
    ///
    /// Gibt den Git-Fehler unverpackt zurück, damit [`GitStore::put_bytes`] den
    /// Wettlauf noch erkennen kann — durch die [`StoreError`]-Fassade hindurch
    /// ginge das nicht mehr.
    fn write_session(
        &self,
        reference: &str,
        session: &SessionBytes,
    ) -> minds_git::Result<RefUpdate> {
        let blob = self.repo.write_blob(session.as_bytes())?;
        let tree = self.repo.write_tree(None, [(SESSION_FILE, blob)])?;
        self.repo
            .commit_tree_to_ref(reference, tree, &commit_message(session.id()))
    }

    /// Legt `bytes` unter `path` im Kontext-Baum ab — ein Blob, in den
    /// bestehenden Baum eingesetzt, als neuer Commit auf dem Ref. Der
    /// gemeinsame Kern von Session-Ablage und Index-Ablage.
    fn write_blob_once(
        &self,
        path: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        let blob = self.repo.write_blob(bytes)?;
        let base = self.repo.tree_at(&self.reference)?;
        let tree = self.repo.write_tree(base, [(path, blob)])?;
        self.repo.commit_tree_to_ref(&self.reference, tree, message)
    }

    /// Legt für `session` einen eigenständigen Ref an, der beim Push als Branch
    /// in der Forge sichtbar wird.
    ///
    /// Der Ref heißt `refs/minds/sessions/<hex>` — unter `refs/minds/`, damit
    /// [`Repo::commit_tree_to_ref`] ihn schreiben darf und `git branch` ihn
    /// nicht zeigt. Sein Baum trägt die Session allein als [`SESSION_BRANCH_FILE`]
    /// und [`SESSION_BRANCH_MD`] (nicht auf den Store-Baum aufgesetzt), und der
    /// Commit ist elternlos: ein Branch, eine Session. `markdown` ist die
    /// gerenderte `session.md`, die die Aufrufstelle beisteuert (der Store selbst
    /// rendert nicht). Idempotent über `commit_tree_to_ref` — zeigt der Ref schon
    /// auf diesen Baum, entsteht nichts, und ein wiederholter Push ist ein No-op.
    pub(crate) fn write_session_branch(
        &self,
        session: &SessionBytes,
        markdown: &str,
    ) -> minds_git::Result<RefUpdate> {
        let reference = session_branch_ref(session.id());
        let json = self.repo.write_blob(session.as_bytes())?;
        let md = self.repo.write_blob(markdown.as_bytes())?;
        let tree = self
            .repo
            .write_tree(None, [(SESSION_BRANCH_FILE, json), (SESSION_BRANCH_MD, md)])?;
        self.repo
            .commit_tree_to_ref(&reference, tree, &commit_message(session.id()))
    }

    /// Ersetzt die Nutzlast eines Session-Refs (heute: durch einen Tombstone).
    ///
    /// Setzt bewusst auf den bisherigen Stand des Refs auf, statt einen neuen
    /// Wurzel-Commit zu schreiben: Das Vergessen ist ein zusätzlicher Commit,
    /// kein Rewrite — die Historie des Refs bezeugt, dass etwas da war.
    fn overwrite_session(
        &self,
        reference: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        self.write_into_session(reference, SESSION_FILE, bytes, message)
    }

    /// Ersetzt den **gesamten** Baum eines Session-Branches durch den Tombstone.
    ///
    /// Anders als [`overwrite_session`](Self::overwrite_session) wird hier nicht
    /// eine Datei getauscht, sondern ein frischer Baum geschrieben: Der Branch
    /// trägt neben der `session.json` eine gerenderte `session.md`, und **beide**
    /// müssen weg. Ein aufgesetzter Baum, der nur `session.json` ersetzte, ließe
    /// den Klartext in `session.md` stehen — genau das Leck, das dieses Kommando
    /// schließt. `write_tree(None, …)` beschreibt den Baum vollständig, es bleibt
    /// keine dritte Datei zurück.
    ///
    /// Wie bei [`overwrite_session`](Self::overwrite_session) wird der Tombstone
    /// **aufgesetzt**, nicht als neuer Wurzel-Commit geschrieben: Der aktuelle
    /// Baum ist inhaltsfrei, der Klartext bleibt aber im Parent-Commit
    /// erreichbar (`<branch>~1:session.md`). Das gilt bewusst für **alle** drei
    /// Orte gleich und wird gesammelt in Issue #14 auf elternlose Tombstones
    /// umgestellt — hier den Branch als Sonderfall vorzuziehen, hieße zwei
    /// verschiedene Tilg-Semantiken nebeneinander zu haben.
    fn overwrite_session_branch(
        &self,
        reference: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        let tomb = self.repo.write_blob(bytes)?;
        let tree = self.repo.write_tree(
            None,
            [(SESSION_BRANCH_FILE, tomb), (SESSION_BRANCH_MD, tomb)],
        )?;
        self.repo.commit_tree_to_ref(reference, tree, message)
    }
}

/// Der Ref, unter dem die Nutzlast von `id` liegt: `refs/minds/store/<voller
/// Hex-Hash>`.
///
/// Der **volle** Hash, nicht der gekürzte des Browsing-Branches: Das hier ist
/// der maßgebliche Ort, und dort darf die Frage nach Kollisionen gar nicht erst
/// aufkommen.
fn session_ref(id: SessionId) -> String {
    format!("{SESSION_STORE_PREFIX}{}", hex_of(id))
}

/// Die ID hinter einem Session-Ref — `None` für alles, was nicht diesem Muster
/// folgt (ein fremder Ref im Namensraum ist kein Defekt, nur keine Session).
fn id_of_ref(name: &str) -> Option<SessionId> {
    let hex = name.strip_prefix(SESSION_STORE_PREFIX)?;
    let id: SessionId = format!("{SESSION_ID_PREFIX}{hex}").parse().ok()?;
    // Gegenprobe wie in `layout`: Gelesen wird auch Großschreibung, geschrieben
    // wird nur klein — sonst meldete `list` eine ID, deren Ref woanders liegt.
    (hex_of(id) == hex).then_some(id)
}

/// Die Hex-Form einer ID, ohne das `b3-`-Präfix.
fn hex_of(id: SessionId) -> String {
    id.to_string()
        .strip_prefix(SESSION_ID_PREFIX)
        .expect("die Textform einer SessionId trägt immer ihr Präfix")
        .to_owned()
}

/// Der lokale Ref eines Session-Branches: `refs/minds/sessions/<hex>`, mit den
/// ersten [`SESSION_BRANCH_HEX`] Hex-Zeichen der ID. Der Push mappt ihn auf den
/// Remote-Branch `minds/session/<hex>`.
fn session_branch_ref(id: SessionId) -> String {
    let hex: String = id
        .as_bytes()
        .iter()
        .take(SESSION_BRANCH_HEX / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("refs/minds/sessions/{hex}")
}

impl ContextStore for GitStore {
    fn put_bytes(&self, session: &SessionBytes) -> Result<Put> {
        let reference = session_ref(session.id());

        // Liegt dort schon genau das? Dann ist nichts zu tun — und zwar
        // wirklich nichts: kein Objekt, kein Baum, kein Commit. Ein
        // *abweichender* Inhalt fällt hier durch und wird unten überschrieben;
        // siehe Modul-Doku.
        let stored = self
            .repo
            .read_blob_at(&reference, SESSION_FILE)
            .map_err(StoreError::backend)?;
        if stored.as_deref() == Some(session.as_bytes()) {
            return Ok(Put::AlreadyPresent(session.id()));
        }

        let mut attempts_left = PUT_ATTEMPTS;

        loop {
            attempts_left -= 1;
            match self.write_session(&reference, session) {
                Ok(update) => {
                    return Ok(if update.wrote_commit() {
                        Put::Written(session.id())
                    } else {
                        // Zwischen Nachsehen und Schreiben war jemand schneller
                        // — mit demselben Inhalt. Die untere Schicht hat es
                        // gefangen.
                        Put::AlreadyPresent(session.id())
                    });
                }
                // Ein paralleler Lauf war schneller. Verloren ist nichts — der
                // nächste Versuch liest den neuen Stand und setzt darauf auf.
                // Seit jede Session ihren eigenen Ref hat, kann das nur noch
                // zwei Läufe mit *derselben* Session treffen.
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
    }

    fn get_bytes(&self, id: SessionId) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self
            .repo
            .read_blob_at(&session_ref(id), SESSION_FILE)
            .map_err(StoreError::backend)?
        {
            return Ok(Some(bytes));
        }
        // Bestandsrepos: Bis v0.2 lag die Nutzlast im Baum des Kontext-Refs.
        // Der Lesepfad kennt beide Orte, damit ein Repo nach dem Update
        // weiterlesbar bleibt, ohne dass jemand etwas migrieren muss.
        self.repo
            .read_blob_at(&self.reference, &path_of(id))
            .map_err(StoreError::backend)
    }

    fn list(&self) -> Result<Vec<SessionId>> {
        // Der neue Ort: ein Ref je Session.
        let mut ids: BTreeSet<SessionId> = self
            .repo
            .refs_under(SESSION_STORE_PREFIX)
            .map_err(StoreError::backend)?
            .iter()
            .filter_map(|(name, _)| id_of_ref(name))
            .collect();

        // Und der alte: Sessions, die vor dem Umzug im Kontext-Baum landeten.
        // Fremde Einträge überspringen statt melden — `index.json` ist ein
        // vorgesehener Nachbar, kein Defekt (siehe `layout`).
        ids.extend(
            self.repo
                .list_blobs_at(&self.reference)
                .map_err(StoreError::backend)?
                .iter()
                .map(String::as_str)
                .filter_map(id_of_path),
        );

        Ok(ids.into_iter().collect())
    }

    fn link(&self, session: SessionId, commit_hex: &str, evidence: Evidence) -> Result<()> {
        let reference = session_ref(session);
        let mut links = self.links_at(&reference)?;

        // Idempotent, und stärkere Herkunft gewinnt — dieselbe Regel wie in
        // [`CommitIndex::link`], nur auf der Sicht *einer* Session.
        match links.iter_mut().find(|link| link.commit == commit_hex) {
            Some(existing) => {
                if existing.evidence >= evidence {
                    return Ok(());
                }
                existing.evidence = evidence;
            }
            None => links.push(SessionLink {
                commit: commit_hex.to_owned(),
                evidence,
            }),
        }
        links.sort_by(|a, b| a.commit.cmp(&b.commit));

        let bytes = serde_json::to_vec(&links).expect("Kanten serialisieren immer");
        let message = format!("minds: Kante {commit_hex} → {session}");
        self.retry_write(|| {
            self.write_into_session(&reference, SESSION_LINKS_FILE, &bytes, &message)
        })
        .map(|_| ())
    }

    fn index(&self) -> Result<CommitIndex> {
        let mut index = CommitIndex::new();

        // Der neue Ort: die Kanten liegen bei ihrer Session.
        for (name, _) in self
            .repo
            .refs_under(SESSION_STORE_PREFIX)
            .map_err(StoreError::backend)?
        {
            let Some(id) = id_of_ref(&name) else { continue };
            for link in self.links_at(&name)? {
                index.link(link.commit, id, link.evidence);
            }
        }

        // Und der alte: eine `index.json` aus einem Bestandsrepo oder aus dem
        // Import. Beides zusammen ergibt den Index — `link` ist idempotent und
        // lässt die stärkere Herkunft gewinnen, also ist die Reihenfolge egal.
        if let Some(bytes) = self.get_index_bytes()? {
            if let Ok(legacy) = serde_json::from_slice::<CommitIndex>(&bytes) {
                for (hex, links) in legacy.iter() {
                    for link in links {
                        index.link(hex.clone(), link.session, link.evidence);
                    }
                }
            }
        }

        Ok(index)
    }

    fn get_index_bytes(&self) -> Result<Option<Vec<u8>>> {
        self.repo
            .read_blob_at(&self.reference, INDEX_PATH)
            .map_err(StoreError::backend)
    }

    fn put_index_bytes(&self, bytes: &[u8]) -> Result<()> {
        // Anders als eine Session ist der Index nicht content-adressiert: Zwei
        // Läufe schreiben verschiedene Inhalte an denselben Pfad. Deshalb kein
        // „liegt schon so da"-Kurzschluss, aber dieselbe CAS-Retry gegen einen
        // parallelen Schreiber.
        let mut attempts_left = PUT_ATTEMPTS;
        loop {
            attempts_left -= 1;
            match self.write_blob_once(INDEX_PATH, bytes, "minds: Index aktualisiert") {
                Ok(_) => return Ok(()),
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
    }

    /// Ersetzt die Nutzlast an **jedem** Ort, an dem sie liegt.
    ///
    /// Eine Session kann an drei Orten liegen: dem Store-Ref (neu, maßgeblich),
    /// dem **Session-Branch** (browsbar in der Forge, mit `session.json` *und*
    /// gerenderter `session.md`) und dem Kontext-Baum (Bestandsrepos). Ein Repo,
    /// das vor dem Umzug schrieb und danach dieselbe Session erneut ablegte, hat
    /// sie an mehreren. Nur einen zu tilgen wäre die schlimmste Sorte Fehler,
    /// die dieses Kommando machen kann: Es meldete „vergessen", und der Klartext
    /// stünde weiter im anderen Baum — auf der Forge, für jeden mit Repo-Zugriff
    /// lesbar. Deshalb wird hier nicht abgekürzt, sondern jeder Ort geprüft und
    /// getilgt, und `forget` benennt zurück, welche es waren.
    fn forget(&self, id: SessionId, reason: &str) -> Result<Forget> {
        let tomb = crate::tombstone::bytes(reason);
        let message = format!("minds: Session {id} vergessen");
        let mut places = Vec::new();

        // Der maßgebliche Ort. Der Tombstone wird an den Store-Ref **angehängt**
        // — ein Fast-Forward, kein Rewrite: Die Referenz bleibt auflösbar, der
        // Inhalt ist aus dem aktuellen Baum weg.
        let reference = session_ref(id);
        if self.payload_at(
            self.repo
                .read_blob_at(&reference, SESSION_FILE)
                .map_err(StoreError::backend)?,
        ) {
            self.retry_write(|| self.overwrite_session(&reference, &tomb, &message))?;
            places.push(ForgottenPlace::StoreRef);
        }

        // Der browsbare Branch. Ohne diesen Zweig meldete `forget` „vergessen",
        // während `session.md` mit dem vollen Klartext weiter als Forge-Branch
        // stünde — ein DSGVO-Verstoß mit Erfolgsmeldung. Geprüft wird an
        // `session.json`; getilgt werden beide Dateien des Branch-Baums.
        let branch = session_branch_ref(id);
        if self.payload_at(
            self.repo
                .read_blob_at(&branch, SESSION_BRANCH_FILE)
                .map_err(StoreError::backend)?,
        ) {
            self.retry_write(|| self.overwrite_session_branch(&branch, &tomb, &message))?;
            places.push(ForgottenPlace::SessionBranch);
        }

        // Der alte Ort. Eine vor dem Umzug abgelegte Session muss sich löschen
        // lassen, ohne dass jemand sie vorher migriert.
        let path = path_of(id);
        if self.payload_at(
            self.repo
                .read_blob_at(&self.reference, &path)
                .map_err(StoreError::backend)?,
        ) {
            self.retry_write(|| self.write_blob_once(&path, &tomb, &message))?;
            places.push(ForgottenPlace::ContextTree);
        }

        Ok(if places.is_empty() {
            // Nicht da — oder schon ein Tombstone. Beides ist „nichts zu tun".
            Forget::Absent(id)
        } else {
            Forget::Forgotten(id, places)
        })
    }
}

/// Eine Kante aus der Sicht *einer* Session: an welchem Commit sie hängt und
/// woher wir das wissen.
///
/// Die Umkehrung von [`IndexLink`](crate::IndexLink), der die Session nennt,
/// weil er unter dem Commit steht. Hier ist es umgekehrt — die Datei liegt schon
/// bei der Session, also fehlt genau das andere Ende.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SessionLink {
    commit: String,
    evidence: Evidence,
}

impl GitStore {
    /// Die Kanten, die im Baum von `reference` liegen.
    ///
    /// Beschädigtes wird als „keine Kanten" gelesen, nicht als Fehler: Der Index
    /// ist eine heuristische Ergänzung; ein kaputter Eintrag darf `minds show`
    /// nicht abschießen. Nachzugehen ist dem in `minds fsck`.
    fn links_at(&self, reference: &str) -> Result<Vec<SessionLink>> {
        let Some(bytes) = self
            .repo
            .read_blob_at(reference, SESSION_LINKS_FILE)
            .map_err(StoreError::backend)?
        else {
            return Ok(Vec::new());
        };
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    /// Setzt eine Datei in den Baum eines Session-Refs — auf den bisherigen
    /// Stand aufgesetzt, damit die Nutzlast daneben stehen bleibt.
    fn write_into_session(
        &self,
        reference: &str,
        path: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        let blob = self.repo.write_blob(bytes)?;
        let base = self.repo.tree_at(reference)?;
        let tree = self.repo.write_tree(base, [(path, blob)])?;
        self.repo.commit_tree_to_ref(reference, tree, message)
    }

    /// Ob an dieser Stelle eine zu tilgende Nutzlast liegt (kein Tombstone).
    fn payload_at(&self, found: Option<Vec<u8>>) -> bool {
        found.is_some_and(|bytes| crate::tombstone::reason(&bytes).is_none())
    }

    /// Wiederholt einen Schreibvorgang, der einen Wettlauf am Ref verloren hat.
    fn retry_write(
        &self,
        mut write: impl FnMut() -> minds_git::Result<RefUpdate>,
    ) -> Result<RefUpdate> {
        let mut attempts_left = PUT_ATTEMPTS;
        loop {
            attempts_left -= 1;
            match write() {
                Ok(update) => return Ok(update),
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
    }
}

/// Die Message eines Kontext-Commits.
///
/// Sie trägt die ID, damit `git log refs/minds/context` von Hand lesbar und
/// greppbar ist. Verbindlich ist sie nicht — der Record ist der Baum, nicht die
/// Message.
fn commit_message(id: SessionId) -> String {
    format!("minds: Session {id}")
}

#[cfg(test)]
mod tests {
    use minds_core::to_canonical_string;
    use minds_git::DEFAULT_CONTEXT_REF;

    use super::*;
    use crate::fixture::{TempRepo, redacted};

    /// Ein Repository mit einem Code-Commit und einem Store darauf.
    fn fresh_store() -> (TempRepo, GitStore) {
        let fixture = TempRepo::init();
        fixture.write_file("src/lib.rs", "fn main() {}\n");
        fixture.commit("code");

        let repo = Repo::open(fixture.path()).unwrap();
        (fixture, GitStore::new(repo, DEFAULT_CONTEXT_REF))
    }

    // --- Layout im echten Repo -----------------------------------------------

    #[test]
    fn the_session_lands_where_the_layout_says() {
        // Gegenprobe mit echtem git: Der Baum muss für Git wohlgeformt sein,
        // inklusive der Zwischenverzeichnisse.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        let text = id.to_string();
        let hex = text.strip_prefix("b3-").unwrap();
        let reference = session_ref(id);

        // Ein Ref je Session, benannt nach ihrem vollen Hash …
        assert_eq!(reference, format!("refs/minds/store/{hex}"));
        // … und darin genau eine Datei. Der Baum wächst nicht mit dem Store —
        // das ist der ganze Punkt des Umzugs.
        let listed = fixture.git(&["ls-tree", "-r", "--name-only", &reference]);
        assert_eq!(listed.trim(), SESSION_FILE);
    }

    #[test]
    fn what_git_stores_is_the_canonical_json() {
        // Der Vertrag mit dem Ökosystem: im Store liegt lesbares JSON, byte-
        // genau das, dessen Hash die ID ist.
        let (fixture, store) = fresh_store();
        let session = redacted("Retry-Test reparieren");
        let id = store.put(&session).unwrap().id();

        let revision = format!("{}:{SESSION_FILE}", session_ref(id));
        let blob = fixture.git(&["cat-file", "blob", &revision]);

        assert_eq!(blob, to_canonical_string(session.session()).unwrap());
    }

    // --- Der Kern-Loop -------------------------------------------------------

    #[test]
    fn put_then_get_roundtrips_through_git() {
        let (_fixture, store) = fresh_store();
        let session = redacted("Retry-Test reparieren");

        let id = store.put(&session).unwrap().id();

        assert_eq!(store.get(id).unwrap().as_ref(), Some(session.session()));
        assert!(store.exists(id).unwrap());
        assert_eq!(store.list().unwrap(), vec![id]);
    }

    #[test]
    fn a_second_session_keeps_the_first() {
        let (fixture, store) = fresh_store();

        let first = store.put(&redacted("Fall A")).unwrap().id();
        let second = store.put(&redacted("Fall B")).unwrap().id();

        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(store.list().unwrap(), expected);
        assert!(store.get(first).unwrap().is_some());

        // Zwei Sessions, zwei Refs, je ein Commit — und keiner der beiden
        // Schreibvorgänge hat den anderen angefasst. Vor dem Umzug lagen hier
        // zwei Commits auf *einem* Ref, und genau das war die Enge.
        for id in [first, second] {
            assert_eq!(
                fixture
                    .git(&["rev-list", "--count", &session_ref(id)])
                    .trim(),
                "1"
            );
        }
    }

    // --- Dedup per Hash ------------------------------------------------------

    #[test]
    fn putting_the_same_session_twice_writes_no_second_commit() {
        // Dedup per Hash, im echten Repo nachgesehen: ein Blob, ein Commit.
        let (fixture, store) = fresh_store();

        let first = store.put(&redacted("gleicher Inhalt")).unwrap();
        let second = store.put(&redacted("gleicher Inhalt")).unwrap();

        assert!(first.was_written());
        assert!(!second.was_written());
        assert_eq!(first.id(), second.id());
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            fixture
                .git(&["rev-list", "--count", &session_ref(first.id())])
                .trim(),
            "1"
        );
    }

    #[test]
    fn a_repeated_put_writes_no_new_objects_at_all() {
        // Schärfer als „kein zweiter Commit": auch kein Blob und kein Baum.
        // `--batch-all-objects` sieht auch, was kein Ref erreicht — ein
        // weggeworfenes Zwischenergebnis fiele hier auf.
        let (fixture, store) = fresh_store();
        store.put(&redacted("gleicher Inhalt")).unwrap();
        let before = fixture.object_count();

        let again = store.put(&redacted("gleicher Inhalt")).unwrap();

        assert!(!again.was_written());
        assert_eq!(fixture.object_count(), before);
    }

    #[test]
    fn dedup_does_not_depend_on_state_in_the_process() {
        // Ein zweiter Lauf von `minds capture` ist ein neuer Prozess. Was
        // dedupliziert, ist der Inhalt im Repo, nicht ein Gedächtnis im Store.
        let (fixture, store) = fresh_store();
        let first = store.put(&redacted("gleicher Inhalt")).unwrap();

        let other_handle = GitStore::new(Repo::open(fixture.path()).unwrap(), DEFAULT_CONTEXT_REF);
        let second = other_handle.put(&redacted("gleicher Inhalt")).unwrap();

        assert!(first.was_written());
        assert!(!second.was_written());
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn a_session_that_was_tampered_with_is_written_again() {
        // Der Kurzschluss vergleicht Bytes, nicht bloß die Existenz. Wer den
        // Blob im Store ersetzt hat, bekommt beim nächsten `put` den richtigen
        // Inhalt zurück — statt eines „liegt schon da" über etwas Fremdem.
        let (_fixture, store) = fresh_store();
        let session = redacted("Retry-Test reparieren");
        let id = store.put(&session).unwrap().id();

        let repo = store.context_repo();
        let reference = session_ref(id);
        let forged = repo.write_blob(b"{\"gefaelscht\":true}").unwrap();
        let base = repo.tree_at(&reference).unwrap();
        let tree = repo.write_tree(base, [(SESSION_FILE, forged)]).unwrap();
        repo.commit_tree_to_ref(&reference, tree, "von Hand geändert")
            .unwrap();
        assert!(matches!(store.get(id), Err(StoreError::Corrupt { .. })));

        let again = store.put(&session).unwrap();

        assert!(again.was_written());
        assert_eq!(store.get(id).unwrap().as_ref(), Some(session.session()));
    }

    // --- Vergessen (DSGVO) ---------------------------------------------------

    #[test]
    fn forget_replaces_the_payload_with_a_resolvable_tombstone() {
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();

        let forgotten = store.forget(id, "DSGVO").unwrap();
        assert!(forgotten.was_forgotten());

        // Referenz bleibt auflösbar, Inhalt meldet Forgotten.
        assert!(matches!(store.get(id), Err(StoreError::Forgotten { .. })));
        assert!(store.exists(id).unwrap());

        // Der Blob im echten Git trägt den Inhalt nicht mehr — nur den Tombstone.
        let revision = format!("{}:{SESSION_FILE}", session_ref(id));
        let blob = fixture.git(&["cat-file", "blob", &revision]);
        assert!(!blob.contains("streng geheim"), "Inhalt überlebt: {blob}");
        assert!(blob.contains("minds_tombstone"));

        // Append-only: das Vergessen ist ein zusätzlicher Commit, kein Rewrite.
        assert_eq!(
            fixture
                .git(&["rev-list", "--count", &session_ref(id)])
                .trim(),
            "2"
        );
    }

    // --- Vor dem ersten capture ----------------------------------------------

    #[test]
    fn a_repository_without_the_ref_is_an_empty_store() {
        // Der Zustand jedes Repos, das Minds noch nie benutzt hat — kein
        // Fehler, sondern leer.
        let (_fixture, store) = fresh_store();
        let never_stored = redacted("nie gespeichert").session().id().unwrap();

        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.get(never_stored).unwrap(), None);
        assert!(!store.exists(never_stored).unwrap());
    }

    // --- Nachbarn ------------------------------------------------------------

    #[test]
    fn list_ignores_entries_that_are_not_sessions() {
        // `index.json` kommt mit dem Reader (M7) dazu und darf die Liste nicht
        // verfälschen.
        let (_fixture, store) = fresh_store();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        let repo = store.context_repo();
        let blob = repo.write_blob(b"{}").unwrap();
        let base = repo.tree_at(store.reference()).unwrap();
        let tree = repo.write_tree(base, [("index.json", blob)]).unwrap();
        repo.commit_tree_to_ref(store.reference(), tree, "minds: Index")
            .unwrap();

        assert_eq!(store.list().unwrap(), vec![id]);
    }

    #[test]
    fn the_index_round_trips_and_coexists_with_sessions() {
        let (_fixture, store) = fresh_store();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        // Frisch: kein Index.
        assert_eq!(store.get_index_bytes().unwrap(), None);
        assert!(store.index().unwrap().is_empty());

        let mut index = crate::CommitIndex::new();
        index.link("deadbeef", id, minds_core::Evidence::Inferred);
        store.set_index(&index).unwrap();

        // Gelesen wie geschrieben, und die Session ist unversehrt daneben.
        assert_eq!(store.index().unwrap(), index);
        assert_eq!(
            store.list().unwrap(),
            vec![id],
            "Index verdrängt keine Session"
        );
        assert!(store.get(id).unwrap().is_some());

        // Überschreiben ersetzt, dupliziert nicht.
        index.link("cafe", id, minds_core::Evidence::Inferred);
        store.set_index(&index).unwrap();
        assert_eq!(store.index().unwrap().len(), 2);
    }

    // --- Kanten bei ihrer Session --------------------------------------------

    #[test]
    fn a_link_lands_at_its_session_and_shows_up_in_the_index() {
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        store.link(id, "deadbeef", Evidence::Observed).unwrap();

        // Im Baum der Session, nicht in einer gemeinsamen Datei.
        let files = fixture.git(&["ls-tree", "-r", "--name-only", &session_ref(id)]);
        assert!(files.contains(SESSION_LINKS_FILE), "{files}");
        assert_eq!(
            store.get_index_bytes().unwrap(),
            None,
            "der gemeinsame Index darf im heißen Pfad nicht mehr angefasst werden"
        );

        // Und der Gesamtindex ist die Vereinigung über die Session-Refs.
        let index = store.index().unwrap();
        assert_eq!(index.links_of("deadbeef").len(), 1);
        assert_eq!(index.links_of("deadbeef")[0].session, id);
        assert_eq!(index.links_of("deadbeef")[0].evidence, Evidence::Observed);
    }

    #[test]
    fn two_sessions_never_touch_the_same_ref_when_linking() {
        // Die eigentliche Zusage für eine Agent-Flotte: Zwei Checkpoints
        // gleichzeitig fassen verschiedene Refs an, also kann es keinen
        // Wettlauf geben.
        let (fixture, store) = fresh_store();
        let first = store.put(&redacted("Fall A")).unwrap().id();
        let second = store.put(&redacted("Fall B")).unwrap().id();

        store.link(first, "cafe", Evidence::Observed).unwrap();
        store.link(second, "cafe", Evidence::Inferred).unwrap();

        // Beide Kanten stehen im selben Commit-Eintrag …
        assert_eq!(store.index().unwrap().links_of("cafe").len(), 2);
        // … aber jede in ihrem eigenen Ref, mit genau einem zusätzlichen Commit.
        for id in [first, second] {
            assert_eq!(
                fixture
                    .git(&["rev-list", "--count", &session_ref(id)])
                    .trim(),
                "2"
            );
        }
    }

    #[test]
    fn linking_is_idempotent_and_the_stronger_evidence_wins() {
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("Retry-Test reparieren")).unwrap().id();

        store.link(id, "cafe", Evidence::Inferred).unwrap();
        let after_first = fixture.hash(&session_ref(id));

        // Dieselbe Kante nochmal: kein Commit.
        store.link(id, "cafe", Evidence::Inferred).unwrap();
        assert_eq!(fixture.hash(&session_ref(id)), after_first);

        // Schwächere Herkunft: ebenfalls kein Commit.
        store.link(id, "cafe", Evidence::Inferred).unwrap();
        assert_eq!(fixture.hash(&session_ref(id)), after_first);

        // Stärkere Herkunft gewinnt.
        store.link(id, "cafe", Evidence::Observed).unwrap();
        assert_ne!(fixture.hash(&session_ref(id)), after_first);
        assert_eq!(
            store.index().unwrap().links_of("cafe")[0].evidence,
            Evidence::Observed
        );
    }

    #[test]
    fn a_legacy_index_is_still_read() {
        // Bestandsrepos und der Import legen weiter eine gemeinsame
        // `index.json` ab. Der Gesamtindex ist die Vereinigung aus beidem.
        let (_fixture, store) = fresh_store();
        let old = store.put(&redacted("Fall A")).unwrap().id();
        let new = store.put(&redacted("Fall B")).unwrap().id();

        let mut legacy = crate::CommitIndex::new();
        legacy.link("cafe", old, Evidence::Inferred);
        store.set_index(&legacy).unwrap();
        store.link(new, "cafe", Evidence::Observed).unwrap();

        let index = store.index().unwrap();
        let sessions: BTreeSet<SessionId> = index
            .links_of("cafe")
            .iter()
            .map(|link| link.session)
            .collect();
        assert_eq!(sessions, BTreeSet::from([old, new]));
    }

    // --- Punkt 8 der Definition of Done --------------------------------------

    #[test]
    fn the_context_ref_stays_invisible_to_normal_git_usage() {
        let (fixture, store) = fresh_store();
        store.put(&redacted("Retry-Test reparieren")).unwrap();

        let branches = fixture.git(&["branch", "--list"]);
        assert!(
            !branches.contains("minds"),
            "sichtbar in git branch: {branches}"
        );
        // Auch der neue Namensraum bleibt außerhalb von refs/heads/ — eine
        // Forge kann ihn weder als Default-Branch wählen noch in die
        // Branch-Liste des Nutzers stellen.
        let refs = fixture.git(&["for-each-ref", "--format=%(refname)", "refs/minds/"]);
        assert!(refs.contains("refs/minds/store/"), "{refs}");
        assert!(!refs.contains("refs/heads/"), "{refs}");
    }

    #[test]
    fn the_context_history_never_touches_the_code_history() {
        // Orphan: Wer nur den Kontext-Ref fetcht, zieht keinen Quellcode mit.
        let (fixture, store) = fresh_store();
        store.put(&redacted("Retry-Test reparieren")).unwrap();

        let id = store.list().unwrap()[0];
        let parents = fixture.git(&["log", "--format=%P", "-1", &session_ref(id)]);
        assert!(
            parents.trim().is_empty(),
            "Wurzel-Commit hat Eltern: {parents}"
        );

        let files = fixture.git(&["ls-tree", "-r", "--name-only", &session_ref(id)]);
        assert!(
            !files.contains("src/lib.rs"),
            "Code im Kontext-Baum: {files}"
        );
    }

    // --- Leitplanke ----------------------------------------------------------

    #[test]
    fn a_store_outside_the_minds_namespace_cannot_write() {
        // Die Regel aus `minds-git` muss durch den Store hindurch halten: ein
        // falsch konfigurierter Ref darf `main` nicht bewegen.
        let (fixture, store) = fresh_store();
        let before = fixture.hash("refs/heads/main");
        let store = store.with_ref("refs/heads/main");

        // Die Nutzlast landet seit dem Umzug ohnehin unter
        // `refs/minds/store/…` — ein falsch konfigurierter Store-Ref kann sie
        // gar nicht mehr woanders hinschreiben. Das ist die stärkere Zusage.
        store.put(&redacted("darf nicht")).unwrap();
        assert_eq!(fixture.hash("refs/heads/main"), before);

        // Was den konfigurierten Ref *doch* anfasst — der Index —, wird von der
        // Leitplanke in `minds-git` abgewiesen.
        let err = store.set_index(&crate::CommitIndex::new()).unwrap_err();
        assert!(
            matches!(err, StoreError::Backend { .. }),
            "erwartet Backend, war: {err:?}"
        );
        assert_eq!(fixture.hash("refs/heads/main"), before);
    }
}
