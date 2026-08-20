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
//! **Ein Tombstone ist die eine Ausnahme, die nicht überschrieben wird.** Er
//! weicht per Konstruktion von der Session ab, fiele also durch denselben
//! Byte-Vergleich und würde als „Fremdes, das repariert werden muss" mit dem
//! Klartext ersetzt — und reanimierte damit eine [`vergessene`](ContextStore::forget)
//! Session beim nächsten Capture. Deshalb schreibt [`GitStore::put_bytes`] über
//! [`GitStore::write_session`] nur mit einem **atomaren Guard**
//! ([`Repo::commit_tree_to_ref_unless`]): Der Tombstone wird am selben Parent
//! geprüft, auf den der Compare-and-Swap aufsetzt — ein `forget` im Fenster löst
//! `RefRaced` aus, der Retry sieht den Tombstone. Bei [`Put::Forgotten`] hält der
//! Store an: Die DSGVO-Löschung überlebt einen wiederholten Capture, eine zweite
//! Maschine und einen erneuten Import.
//!
//! Der browsbare Session-Branch (`put_session_branch`) ist der **zweite** Weg zur
//! Forge und braucht denselben Schutz — nur lässt er sich nicht in *einem*
//! atomaren Schritt geben, weil sein Tombstone-Kriterium am *Store-Ref* hängt und
//! Gits CAS per-Ref ist. [`GitStore::put_session_branch_bytes`] staffelt ihn
//! deshalb: atomarer Guard gegen den Branch-eigenen Tombstone **plus** ein
//! Post-Check gegen den Store-Ref, der einen im Rennen zurückgebliebenen
//! Klartext-Branch nachträglich tilgt.
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
use minds_git::{CommitId, GitError, MINDS_REF_NAMESPACE, RefUpdate, Repo};

use crate::bytes::SessionBytes;
use crate::error::{Result, StoreError};
use crate::index::CommitIndex;
use crate::layout::{id_of_path, path_of};
use crate::store::{ContextStore, Forget, ForgottenPlace, Put};

/// Wie oft [`GitStore::put_bytes`] einen verlorenen Wettlauf am Ref wiederholt,
/// bevor er ihn meldet.
///
/// Zehn statt der historischen drei: Seit der Compare-and-Swap in `minds-git`
/// wirklich durchgesetzt wird (#4 — vorher schluckte ein Verify-vor-dem-Lock
/// in gix die Konflikte still), sind verlorene Wettläufe der **normale**
/// Ausgang unter Last, kein Randfall. Jeder Versuch liest frisch, mergt neu
/// und ist Millisekunden kurz — die Schranke ist ein Notausgang gegen einen
/// Ref, der sich nie beruhigt, keine Fairness-Annahme.
const PUT_ATTEMPTS: u32 = 10;

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
///   sich danach nicht mehr — außer beim Vergessen, das ihn auf einen
///   elternlosen Tombstone setzt (#14); den überträgt `minds sync` als gezielten
///   Force-Push (#102). Regulär kann er nicht non-fast-forward abprallen.
///
/// `refs/minds/store/` und nicht `refs/minds/sessions/`: Letzteres trägt die
/// *browsbaren* Branches des Child-Backends (gekürzter Hash, mit `session.md`
/// zum Anschauen). Die Nutzlast und die Ansicht sind zwei Dinge; sie sollen
/// nicht im selben Namensraum liegen.
const SESSION_STORE_PREFIX: &str = "refs/minds/store/";

/// Der Namensraum der eigenen Push-Buchhaltung: `refs/minds/remotes/<remote>/…`.
///
/// Geschrieben wird hier nur in `minds-cli/sync` nach einem bestätigten Push (ein
/// Tracking-Ref auf den gepushten Commit). `forget` muss diese Refs
/// **mit**behandeln: Zeigt einer noch auf den Klartext-Commit, hielte er ihn
/// erreichbar und gc-immun, obwohl der maßgebliche Ref längst ein Tombstone ist
/// (#14). Deshalb lebt die Konstante hier — beim Store, der die Löschung
/// vollständig machen muss — und `minds-cli` bezieht sie von hier, damit beide
/// Seiten dieselbe Konvention teilen.
pub const TRACKING_REF_PREFIX: &str = "refs/minds/remotes/";

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

/// Der Namensraum der browsbaren Session-Branches: `refs/minds/sessions/<hex>`
/// (gekürzter Hash). Der Push mappt sie im Child-Backend auf Branches
/// `minds/session/<hex>`, damit die Forge jede Session als Seite zeigt.
const SESSION_BRANCH_PREFIX: &str = "refs/minds/sessions/";

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

/// Ein Tilgungs-Ort für [`GitStore::forget_one`]: welcher der drei Orte, unter
/// welchem Ref er liegt und in welcher Datei sein Payload steht. Gebündelt, damit
/// `forget_one` nicht an einer langen Argumentliste hängt.
struct Site<'a> {
    place: ForgottenPlace,
    reference: &'a str,
    file: &'a str,
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

    /// Ein Schreibversuch für die Nutzlast einer Session — außer am Ref liegt
    /// bereits ein Tombstone; dann gibt der Guard `None` zurück und die Session
    /// bleibt vergessen (#6). Sonst: Blob, Baum, Commit, **eigener** Ref.
    ///
    /// Der Tombstone-Check läuft **atomar** mit dem Compare-and-Swap: Er prüft
    /// den Blob am selben Parent, auf den aufgesetzt würde. Ein `forget`, das
    /// zwischen Prüfung und Commit landet, löst `RefRaced` aus — der Aufrufer
    /// wiederholt und sieht den Tombstone. So kann kein nebenläufiger Lauf den
    /// Klartext auf einen Tombstone aufsetzen.
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
    ) -> minds_git::Result<Option<RefUpdate>> {
        let blob = self.repo.write_blob(session.as_bytes())?;
        let tree = self.repo.write_tree(None, [(SESSION_FILE, blob)])?;
        self.repo.commit_tree_to_ref_unless(
            reference,
            tree,
            SESSION_FILE,
            |bytes| crate::tombstone::reason(bytes).is_some(),
            &commit_message(session.id()),
        )
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

    /// Legt für `session` den browsbaren Branch an — es sei denn, er trägt schon
    /// einen Tombstone. Reanimations-Schutz und Retry stecken in
    /// [`put_session_branch_bytes`](Self::put_session_branch_bytes); dies ist der
    /// nackte Schreibvorgang darunter.
    ///
    /// Der Ref heißt `refs/minds/sessions/<hex>` — unter `refs/minds/`, damit
    /// [`Repo::commit_tree_to_ref_unless`] ihn schreiben darf und `git branch` ihn
    /// nicht zeigt. Sein Baum trägt die Session allein als [`SESSION_BRANCH_FILE`]
    /// und [`SESSION_BRANCH_MD`] (nicht auf den Store-Baum aufgesetzt), und der
    /// Commit ist elternlos: ein Branch, eine Session. `markdown` ist die
    /// gerenderte `session.md`, die die Aufrufstelle beisteuert (der Store selbst
    /// rendert nicht).
    ///
    /// Der Guard prüft [`SESSION_BRANCH_FILE`] am selben Parent, auf den der CAS
    /// aufsetzt: Trägt der Branch dort einen Tombstone, kommt `Ok(None)` und
    /// nichts wird geschrieben — Klartext wird **nie** über einen getilgten Branch
    /// gesetzt, auch nicht, wenn ein `forget` zwischen Vor-Check und Commit landet
    /// (dann `RefRaced`, der Retry sieht den Tombstone). Zeigt der Ref schon auf
    /// diesen Baum, entsteht nichts, und ein wiederholter Push ist ein No-op.
    pub(crate) fn write_session_branch(
        &self,
        session: &SessionBytes,
        markdown: &str,
    ) -> minds_git::Result<Option<RefUpdate>> {
        let reference = session_branch_ref(session.id());
        let json = self.repo.write_blob(session.as_bytes())?;
        let md = self.repo.write_blob(markdown.as_bytes())?;
        let tree = self
            .repo
            .write_tree(None, [(SESSION_BRANCH_FILE, json), (SESSION_BRANCH_MD, md)])?;
        self.repo.commit_tree_to_ref_unless(
            &reference,
            tree,
            SESSION_BRANCH_FILE,
            |bytes| crate::tombstone::reason(bytes).is_some(),
            &commit_message(session.id()),
        )
    }

    /// Legt den browsbaren Session-Branch an — mit dem vollen Reanimations-Schutz
    /// aus #6. Der Weg, den [`ContextStore::put_session_branch`] geht.
    ///
    /// Der Branch ist der **zweite** Weg auf die Forge (neben dem Store-Ref), und
    /// eine vergessene Session darf über ihn nicht als Klartext-`session.md`
    /// zurückkehren. Anders als der Store-Ref lässt sich der Branch **nicht** in
    /// einem einzigen atomaren Schritt schützen: Sein Tombstone-Kriterium hängt am
    /// *Store-Ref*, und Gits Compare-and-Swap ist per-Ref — den Store-Ref-Stand
    /// kann der Commit auf den Branch-Ref nicht mitprüfen. Der Schutz ist deshalb
    /// dreifach gestaffelt:
    ///
    /// 1. **Vor-Check.** Ist die Session schon vergessen, wird gar nicht erst
    ///    geschrieben — und ein etwa aus einem Rennen zurückgebliebener
    ///    Klartext-Branch sofort mitgetilgt.
    /// 2. **Atomarer Branch-Guard.** [`write_session_branch`](Self::write_session_branch)
    ///    setzt Klartext nie über einen Branch-eigenen Tombstone (`RefRaced`-Retry
    ///    inklusive) — das schließt den Fall, dass ein `forget` den *Branch* tilgt,
    ///    während dieser Aufruf läuft.
    /// 3. **Post-Check.** Nach dem Schreiben wird der Store-Ref erneut geprüft.
    ///    Hat ein `forget` ihn getombsteint, nachdem der Vor-Check ihn noch als
    ///    Klartext sah (der Branch existierte da noch nicht, `forget` sah ihn also
    ///    nicht), wird der eben angelegte Branch hier selbst getilgt.
    ///
    /// Zusammen decken 2 und 3 jede Verschränkung von Capture und `forget` ab:
    /// `forget` tombsteint den Store-Ref **vor** dem Branch-Scan, dieser Pfad
    /// prüft den Store-Ref **nach** dem Branch-Schreiben — beide „nach"-Kanten
    /// kreuzen sich, ein Klartext-Branch bleibt an keiner Reihenfolge zurück.
    pub(crate) fn put_session_branch_bytes(
        &self,
        session: &SessionBytes,
        markdown: &str,
    ) -> Result<()> {
        let id = session.id();

        // 1. Schon vergessen: nicht schreiben — und einen Klartext-Branch, der
        //    aus einem Rennen zurückblieb, jetzt tilgen.
        if let Some(reason) = self.forgotten_reason(id)? {
            return self.tombstone_branch_if_plaintext(id, &reason);
        }

        // 2. Schreiben, atomar gegen einen Branch-eigenen Tombstone, mit Retry.
        let mut attempts_left = PUT_ATTEMPTS;
        loop {
            attempts_left -= 1;
            match self.write_session_branch(session, markdown) {
                // Der Branch-Parent trägt einen Tombstone — bleibt vergessen.
                Ok(None) => return Ok(()),
                Ok(Some(_)) => break,
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }

        // 3. Post-Check gegen den maßgeblichen Store-Ref.
        if let Some(reason) = self.forgotten_reason(id)? {
            self.tombstone_branch_if_plaintext(id, &reason)?;
        }
        Ok(())
    }

    /// Tilgt den Session-Branch von `id`, falls er dort noch Klartext trägt.
    ///
    /// Ein bereits getombsteinter oder gar nicht vorhandener Branch ist ein
    /// No-op. Der `reason` landet im Tombstone, damit der Branch denselben Grund
    /// nennt wie der maßgebliche Store-Ref.
    fn tombstone_branch_if_plaintext(&self, id: SessionId, reason: &str) -> Result<()> {
        let reference = session_branch_ref(id);
        let stored = self
            .repo
            .read_blob_at(&reference, SESSION_BRANCH_FILE)
            .map_err(StoreError::backend)?;
        if self.payload_at(stored) {
            let tomb = crate::tombstone::bytes(reason);
            let message = format!("minds: Session {id} vergessen");
            self.retry_write(|| self.overwrite_session_branch(&reference, &tomb, &message))?;
        }
        Ok(())
    }

    /// Der Tilgungsgrund von `id`, falls sie vergessen wurde — sonst `None`.
    ///
    /// Geprüft wird zuerst der maßgebliche Store-Ref, dann der browsbare
    /// Session-Branch; der erste gefundene Tombstone gewinnt. Der Reanimations-
    /// Schutz aus `put_bytes` sitzt am Store-Ref; der Branch ist ein zweiter Weg,
    /// auf dem ein wiederholter Capture den Klartext zurück auf die Forge
    /// schriebe. `put_session_branch` fragt deshalb hier, bevor es schreibt.
    ///
    /// Entscheidend ist der **Store-Ref**, nicht nur der Branch: Eine per
    /// `import` abgelegte Session hat keinen Branch. `forget` tombsteint dann nur
    /// den Store-Ref — würde hier bloß der Branch geprüft, käme `None` heraus,
    /// und ein späterer Capture legte den Branch mit Klartext neu an. Die Session
    /// gälte als vergessen, läge aber wieder browsbar auf der Forge (#6). Ein
    /// Tombstone an *einem* der beiden Orte genügt darum, den Branch-Schreibweg zu
    /// sperren.
    ///
    /// Der Grund selbst dient als Tombstone-Text, wenn ein im Rennen frisch
    /// angelegter Branch nachträglich getilgt werden muss (siehe
    /// [`put_session_branch_bytes`](Self::put_session_branch_bytes)) — so trägt
    /// der Branch-Tombstone denselben Grund wie der maßgebliche Store-Ref.
    pub(crate) fn forgotten_reason(&self, id: SessionId) -> Result<Option<String>> {
        for (reference, file) in [
            (session_ref(id), SESSION_FILE),
            (session_branch_ref(id), SESSION_BRANCH_FILE),
        ] {
            if let Some(bytes) = self
                .repo
                .read_blob_at(&reference, file)
                .map_err(StoreError::backend)?
            {
                if let Some(reason) = crate::tombstone::reason(&bytes) {
                    return Ok(Some(reason));
                }
            }
        }
        Ok(None)
    }

    /// Ersetzt die Nutzlast eines Session-Refs durch einen Tombstone — als
    /// **elternlosen** Wurzel-Commit (#14).
    ///
    /// Ein aufgesetzter Tombstone ließe den alten `session.json`-Klartext über
    /// `<ref>~1` regulär erreichbar, und er reiste bei jedem Push mit — die
    /// DSGVO-Löschung wäre kosmetisch. Deshalb kappt die Tilgung die Historie:
    /// [`Repo::reset_ref_to_root`] schreibt den neuen Baum ohne Eltern. Der
    /// aktuelle Baum wird als Basis genommen, damit Nebendateien (etwa
    /// [`SESSION_LINKS_FILE`]) erhalten bleiben; nur `session.json` wird zum
    /// Tombstone, und alle früheren Stände fallen weg.
    fn overwrite_session(
        &self,
        reference: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        self.reset_root_with_file(reference, SESSION_FILE, bytes, message)
    }

    /// Ersetzt den **gesamten** Baum eines Session-Branches durch den Tombstone —
    /// als elternlosen Wurzel-Commit (#14).
    ///
    /// Zwei Gründe für den frischen Baum: Der Branch trägt neben der
    /// `session.json` eine gerenderte `session.md`, und **beide** müssen weg — ein
    /// aufgesetzter Baum, der nur `session.json` ersetzte, ließe den Klartext in
    /// `session.md` stehen. `write_tree(None, …)` beschreibt den Baum vollständig,
    /// es bleibt keine dritte Datei zurück.
    ///
    /// Und wie bei [`overwrite_session`](Self::overwrite_session) wird der
    /// Tombstone elternlos geschrieben: Der alte Klartext bliebe sonst über
    /// `<branch>~1:session.md` erreichbar. [`Repo::reset_ref_to_root`] kappt die
    /// Historie des Branch-Refs — eine private Orphan-Kette mit genau einer
    /// Session, deren Rewrite billig ist.
    fn overwrite_session_branch(
        &self,
        reference: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        // Den Branch-Ref einmal auflösen und denselben Stand als CAS-Erwartung
        // übergeben (#14, B2): Bei einem verlorenen Wettlauf meldet
        // `reset_ref_to_root` `RefRaced`, und `retry_write` setzt frisch auf.
        let current = self.repo.commit_at(reference)?;
        let tomb = self.repo.write_blob(bytes)?;
        let tree = self.repo.write_tree(
            None,
            [(SESSION_BRANCH_FILE, tomb), (SESSION_BRANCH_MD, tomb)],
        )?;
        self.repo
            .reset_ref_to_root(reference, tree, current, message)
    }

    /// Tauscht `path` im aktuellen Baum von `reference` gegen `bytes` und setzt
    /// den Ref als **elternlosen** Wurzel-Commit neu (#14).
    ///
    /// Der Weg, auf dem `forget` einen Tombstone so setzt, dass der alte Inhalt
    /// von `path` über keinen Ref mehr erreichbar ist. Anders als das reguläre
    /// Fortschreiben (Kanten via `update_blob_in_ref`, das auf den Stand
    /// **aufsetzt**) fällt hier die Historie weg. Der aktuelle Baum ist die Basis, damit die übrigen Einträge
    /// (andere Sessions im Kontext-Baum, Nebendateien am Store-Ref) im aktuellen
    /// Stand erhalten bleiben — nur ihre Historie geht mit, nicht ihr Inhalt.
    ///
    /// Der Ref wird **einmal** aufgelöst (`current`), und aus genau diesem Commit
    /// stammen sowohl die Baum-Basis als auch die CAS-Erwartung an
    /// [`reset_ref_to_root`](minds_git::Repo::reset_ref_to_root). Das schließt die
    /// Lücke, in der ein nebenläufiger `forget` auf dem geteilten Kontext-Baum
    /// zwischen „Basis lesen" und „CAS prüfen" den Ref bewegt und so eine
    /// Klartext-Auferstehung festschreibt (#14, B2): Bewegt sich der Ref, schlägt
    /// die CAS fehl (`RefRaced`), und `retry_write` liest Basis und Erwartung
    /// frisch.
    fn reset_root_with_file(
        &self,
        reference: &str,
        path: &str,
        bytes: &[u8],
        message: &str,
    ) -> minds_git::Result<RefUpdate> {
        let current = self.repo.commit_at(reference)?;
        let base = current.map(|c| self.repo.tree_of(c)).transpose()?;
        let blob = self.repo.write_blob(bytes)?;
        let tree = self.repo.write_tree(base, [(path, blob)])?;
        self.repo
            .reset_ref_to_root(reference, tree, current, message)
    }

    /// Löst die Push-Buchhaltung eines Orts vom Klartext: **löscht** jeden
    /// `refs/minds/remotes/<remote>/<rest>`, der einen session-exklusiven Ref
    /// ankert und dabei abgeschnittenen Klartext trägt — beim geteilten
    /// Kontext-Ref setzt er ihn stattdessen auf den aktuellen Stand um. Gibt
    /// zurück, ob etwas verändert wurde.
    ///
    /// Nach dem Reset trägt der maßgebliche Ref einen elternlosen Tombstone; der
    /// Klartext ist über ihn nicht mehr erreichbar. Ein Tracking-Ref aber, den
    /// `minds sync` nach einem früheren Push anlegte, zeigt **weiter** auf den
    /// Klartext-Commit und hielte ihn `gc`-immun erreichbar — die DSGVO-Löschung
    /// bliebe lokal unvollständig, obwohl `rev-list` über die Session-Refs sauber
    /// aussieht.
    ///
    /// **Löschen, nicht umsetzen (session-exklusive Refs, #102):** Der gelöschte
    /// Tracking-Ref entankert den Klartext **und** lässt `minds sync` den Ref
    /// wieder anbieten — der sieht am ungetrackten Ref einen Tombstone und
    /// überträgt genau ihn per gezieltem Force-Push zur Forge. Würde der
    /// Tracking-Ref stattdessen auf den Tombstone umgesetzt, sähe `sync`
    /// `tracked == local` und böte nichts an; die Forge behielte den Klartext
    /// als aktuelle Ref-Spitze (browsbare `session.md`), obwohl `forget` Erfolg
    /// gemeldet hat.
    ///
    /// **Der geteilte Kontext-Ref wird weiter umgesetzt:** Sein Baum gehört
    /// nicht einer Session allein, und `sync` kann seine Spitze nicht als
    /// Tombstone verifizieren — ein Force-Push wäre dort nicht abgrenzbar und
    /// könnte fremde Stände überschreiben. Umgesetzt ankert der Tracking-Ref
    /// keinen Klartext mehr; die Remote-Historie des Kontext-Refs nachzuziehen
    /// bleibt ein manueller Schritt.
    ///
    /// **Nur echte Klartext-Anker, kein Fast-Forward-Rückstand:** Umgesetzt wird
    /// nur, wenn der Tracking-Stand **kein** Vorfahr des aktuellen ist — dann hat
    /// ein Orphan-Reset die Kette gekappt und der alte Stand ist unerreichbarer
    /// Klartext. Liegt der Tracking-Ref bloß fast-forward zurück (der geteilte
    /// Kontext-Ref, den eine *andere* Session vorwärtsschrieb), gehört der
    /// Rückstand `minds sync`; ihn anzufassen verschlänge einen legitimen Push.
    ///
    /// **Bedingungslos, nicht an `payload_at` gekoppelt:** Schlug ein früheres
    /// Umsetzen fehl, ist der maßgebliche Ref längst ein Tombstone (kein Payload
    /// mehr), der Tracking-Ref hinge aber noch am Klartext. Liefe dieser Schritt
    /// nur bei vorhandenem Payload, könnte ein zweiter `forget` den Leak nicht mehr
    /// heilen. Darum läuft er für jeden Ort, unabhängig vom Tilgungs-Guard.
    fn retarget_tracking(&self, reference: &str) -> Result<bool> {
        let Some(rest) = reference.strip_prefix(MINDS_REF_NAMESPACE) else {
            return Ok(false);
        };
        let target = self
            .repo
            .commit_at(reference)
            .map_err(StoreError::backend)?;
        let mut changed = false;
        for (name, current) in self
            .repo
            .refs_under(TRACKING_REF_PREFIX)
            .map_err(StoreError::backend)?
        {
            // `name` = refs/minds/remotes/<remote>/<rest>. Der Remote-Name darf
            // Schrägstriche enthalten, deshalb über das Suffix statt `split_once`:
            // getroffen wird, was hinter *irgendeinem* Remote-Segment genau `rest`
            // trägt.
            let anchors_same_place = name
                .strip_prefix(TRACKING_REF_PREFIX)
                .and_then(|after| after.strip_suffix(rest))
                .and_then(|remote_slash| remote_slash.strip_suffix('/'))
                .is_some_and(|remote| !remote.is_empty());
            if !anchors_same_place {
                continue;
            }
            match target {
                Some(head) if current != head => {
                    // Nur anfassen, wenn `current` **kein** Vorfahr von `head`
                    // ist: Dann hat ein Orphan-Reset (Tombstone) die Kette gekappt,
                    // und der Tracking-Ref ankert abgeschnittenen (Klartext-)Inhalt
                    // — de-ankern. Ist `current` dagegen ein Vorfahr (der geteilte
                    // Kontext-Ref wanderte bloß fast-forward vorwärts, weil eine
                    // *andere* Session ihn fortschrieb), gehört der Rückstand
                    // `minds sync`, nicht `forget`; ihn anzufassen risse einen
                    // legitimen, noch ausstehenden Push weg (#14).
                    if !self
                        .repo
                        .is_ancestor(current, head)
                        .map_err(StoreError::backend)?
                    {
                        if session_exclusive(rest) {
                            // De-ankern durch Löschen: `sync` sieht den Ref als
                            // ungetrackt, erkennt den Tombstone an der Spitze und
                            // trägt die Löschung per Force-Push zur Forge (#102).
                            self.repo.delete_ref(&name).map_err(StoreError::backend)?;
                        } else {
                            self.repo
                                .set_ref(&name, head)
                                .map_err(StoreError::backend)?;
                        }
                        changed = true;
                    }
                }
                Some(_) => {}
                // Kein maßgeblicher Ref mehr — der Tracking-Ref ist verwaist.
                None => {
                    self.repo.delete_ref(&name).map_err(StoreError::backend)?;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    /// Tilgt **einen** Ort und löst dessen Push-Buchhaltung vom Klartext — der
    /// wiederholte Baustein von [`forget_guarded`](Self::forget_guarded).
    ///
    /// Trägt der Ort noch Payload, wird er getilgt (mit Guard und Retry). Danach
    /// läuft [`retarget_tracking`](Self::retarget_tracking) **immer** — auch wenn
    /// hier nichts getilgt wurde —, damit ein aus einem früheren Fehlschlag
    /// zurückgebliebener Tracking-Ref eingeholt wird. Der Ort gilt als „getilgt"
    /// (und wird gezählt), wenn hier Payload wich **oder** ein Tracking-Ref
    /// umgesetzt/gelöscht wurde.
    fn forget_one(
        &self,
        id: SessionId,
        site: Site<'_>,
        forgotten: &mut Vec<ForgottenPlace>,
        guard: &mut impl FnMut(ForgottenPlace) -> Result<()>,
        write: impl FnMut() -> minds_git::Result<RefUpdate>,
    ) -> Result<()> {
        let Site {
            place,
            reference,
            file,
        } = site;
        let has_payload = self.payload_at(
            self.repo
                .read_blob_at(reference, file)
                .map_err(StoreError::backend)?,
        );
        if has_payload {
            guard(place)
                .and_then(|()| self.retry_write(write))
                .map_err(|source| {
                    StoreError::forget_incomplete(id, forgotten.clone(), place, source)
                })?;
        }
        let retargeted = self.retarget_tracking(reference).map_err(|source| {
            StoreError::forget_incomplete(id, forgotten.clone(), place, source)
        })?;
        if has_payload || retargeted {
            forgotten.push(place);
        }
        Ok(())
    }

    /// Tilgt eine Session an **jedem** Ort, an dem sie liegt — der Rumpf hinter
    /// [`ContextStore::forget`], mit einem einschiebbaren `guard` für Tests.
    ///
    /// Eine Session kann an drei Orten liegen: dem Store-Ref (neu, maßgeblich),
    /// dem Session-Branch (browsbar in der Forge, `session.json` *und*
    /// `session.md`) und dem Kontext-Baum (Bestandsformat). Ein Repo, das vor dem
    /// Umzug schrieb und danach dieselbe Session erneut ablegte, hat sie an
    /// mehreren. Jeder Ort wird als **elternloser** Tombstone getilgt (#14),
    /// sodass der Klartext über keinen Ref mehr erreichbar bleibt.
    ///
    /// Die Tilgung mehrerer Orte ist **nicht** in einer Git-Transaktion atomar:
    /// Bricht sie nach dem ersten Ort ab, sind die schon getilgten weg, der
    /// offene trägt weiter Klartext. Damit das nicht unsichtbar bleibt, meldet der
    /// Fehler [`StoreError::ForgetIncomplete`] die schon getilgten Orte und den
    /// offenen — und ein erneuter `forget` vollendet die Löschung, weil er die
    /// schon getilgten Orte an ihrem Tombstone erkennt und überspringt.
    ///
    /// `guard` läuft vor jedem Schreibschritt; gibt er einen Fehler, wird der wie
    /// ein Schreibfehler an diesem Ort behandelt. In Produktion ist er ein No-op;
    /// Tests nutzen ihn, um einen Abbruch mitten in der Sequenz zu erzwingen.
    pub(crate) fn forget_guarded(
        &self,
        id: SessionId,
        reason: &str,
        mut guard: impl FnMut(ForgottenPlace) -> Result<()>,
    ) -> Result<Forget> {
        let tomb = crate::tombstone::bytes(reason);
        let message = format!("minds: Session {id} vergessen");
        let mut forgotten = Vec::new();

        // Der maßgebliche Ort.
        let reference = session_ref(id);
        self.forget_one(
            id,
            Site {
                place: ForgottenPlace::StoreRef,
                reference: &reference,
                file: SESSION_FILE,
            },
            &mut forgotten,
            &mut guard,
            || self.overwrite_session(&reference, &tomb, &message),
        )?;

        // Der browsbare Branch. Ohne diesen Ort meldete `forget` „vergessen",
        // während `session.md` mit dem vollen Klartext weiter als Forge-Branch
        // stünde. Geprüft an `session.json`, getilgt beide Dateien des Baums.
        let branch = session_branch_ref(id);
        self.forget_one(
            id,
            Site {
                place: ForgottenPlace::SessionBranch,
                reference: &branch,
                file: SESSION_BRANCH_FILE,
            },
            &mut forgotten,
            &mut guard,
            || self.overwrite_session_branch(&branch, &tomb, &message),
        )?;

        // Der alte Ort — geteilter Baum: Der elternlose Reset trägt den
        // vollständigen aktuellen Baum, sodass nur die Historie wegfällt und die
        // übrigen Sessions im aktuellen Stand bleiben.
        let path = path_of(id);
        self.forget_one(
            id,
            Site {
                place: ForgottenPlace::ContextTree,
                reference: &self.reference,
                file: &path,
            },
            &mut forgotten,
            &mut guard,
            || self.reset_root_with_file(&self.reference, &path, &tomb, &message),
        )?;

        Ok(if forgotten.is_empty() {
            // Nicht da — oder schon ein Tombstone ohne Tracking-Rest. Beides ist
            // „nichts zu tun".
            Forget::Absent(id)
        } else {
            Forget::Forgotten(id, forgotten)
        })
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
    format!("{SESSION_BRANCH_PREFIX}{hex}")
}

/// Ob `rest` (der Ref-Name hinter `refs/minds/`) einen Ort bezeichnet, der genau
/// **einer** Session gehört: die Nutzlast (`store/<hex>`) oder ihr browsbarer
/// Branch (`sessions/<hex>`). Nur an solchen Refs ist ein Tombstone die ganze
/// Wahrheit über den Ref — der geteilte Kontext-Ref trägt daneben die übrigen
/// Sessions und fällt hier bewusst durch.
fn session_exclusive(rest: &str) -> bool {
    [SESSION_STORE_PREFIX, SESSION_BRANCH_PREFIX]
        .into_iter()
        .any(|prefix| {
            prefix
                .strip_prefix(MINDS_REF_NAMESPACE)
                .is_some_and(|prefix| rest.starts_with(prefix))
        })
}

/// Der Tombstone-Grund, wenn `commit` an einem **session-exklusiven** Minds-Ref
/// einen Tombstone trägt — sonst `None`.
///
/// Das ist die Weiche, an der `minds sync` entscheidet, ob ein non-fast-forward-
/// Ref per gezieltem Force-Push zur Forge darf (#102): Nur wenn der zu pushende
/// Stand nachweislich ein Tombstone ist, darf er einen fremden Stand ersetzen —
/// nie Klartext über Klartext. Deshalb ist die Prüfung fail-closed: Ein Ref
/// außerhalb der Session-Namensräume, ein unlesbarer Commit oder eine Nutzlast,
/// die kein Tombstone ist, ergeben alle `None`.
pub fn tombstone_at(repo: &Repo, reference: &str, commit: CommitId) -> Option<String> {
    let rest = reference.strip_prefix(MINDS_REF_NAMESPACE)?;
    if !session_exclusive(rest) {
        return None;
    }
    // Elternlos muss er sein: Ein Force-Push überträgt den Commit samt
    // Historie. `forget` schreibt Tombstones nur als Wurzel (#14); trüge einer
    // doch Eltern, reiste deren Inhalt mit — dann lieber zurückstellen.
    if !repo.is_root_commit(commit).ok()? {
        return None;
    }
    // Beide Orte tragen ihre Nutzlast als `session.json`; geprüft wird trotzdem
    // die Datei des jeweiligen Orts, damit ein künftiges Auseinanderlaufen der
    // Konstanten hier nicht stumm danebengriffe.
    let file = if reference.starts_with(SESSION_STORE_PREFIX) {
        SESSION_FILE
    } else {
        SESSION_BRANCH_FILE
    };
    let tree = repo.tree_of(commit).ok()?;
    let bytes = repo.read_blob(tree, file).ok()??;
    crate::tombstone::reason(&bytes)
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

        // Ein Tombstone weicht per Konstruktion von der Session ab — ohne diese
        // Prüfung fiele er durch den Vergleich oben und würde unten mit dem
        // Klartext überschrieben. Genau das reanimierte eine vergessene Session
        // beim nächsten Capture-Lauf. Ein Tombstone ist deshalb kein „anderer
        // Inhalt, also überschreiben", sondern eine Endstation: nicht anfassen.
        //
        // Dieser Vorab-Check ist der schnelle Pfad (spart Blob und Baum, wenn
        // schon offensichtlich vergessen). Er ist *nicht* die Garantie: Zwischen
        // ihm und dem Schreiben könnte ein `forget` landen. Dagegen hält der
        // atomare Guard in [`write_session`](Self::write_session), der den
        // Tombstone am selben Parent prüft, auf den aufgesetzt würde — er liefert
        // `Ok(None)`, wenn der Ref beim Schreiben (oder nach einem `RefRaced`
        // beim erneuten Versuch) einen Tombstone trägt.
        if crate::tombstone::reason(stored.as_deref().unwrap_or_default()).is_some() {
            return Ok(Put::Forgotten(session.id()));
        }

        let mut attempts_left = PUT_ATTEMPTS;

        loop {
            attempts_left -= 1;
            match self.write_session(&reference, session) {
                // Der Guard hat einen Tombstone am Parent gesehen — die Session
                // bleibt vergessen. Das schließt das Fenster zwischen dem
                // Vorab-Check oben und dem Schreiben: ein `forget`, das dazwischen
                // (oder in einem `RefRaced`-Retry) landet, wird hier gefangen.
                Ok(None) => return Ok(Put::Forgotten(session.id())),
                Ok(Some(update)) => {
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
        let message = format!("minds: Kante {commit_hex} → {session}");

        // Lesen, Mergen und Schreiben laufen als **ein** atomarer Schritt über
        // `update_blob_in_ref`: Der Merge arbeitet auf dem Blob genau des
        // Commits, auf den der CAS aufsetzt. Vorher lag der Merge außerhalb —
        // wer das Rennen verlor, schrieb im Retry seine veralteten Bytes über
        // die Kante des Gewinners (Lost Update, #4), und `why`/`show` fanden
        // die Session über diesen Commit nicht mehr. Bei `RefRaced` wird vom
        // neuen Stand aus erneut gemergt.
        let mut attempts_left = PUT_ATTEMPTS;
        let mut corrupt = false;
        loop {
            attempts_left -= 1;
            let outcome =
                self.repo
                    .update_blob_in_ref(&reference, SESSION_LINKS_FILE, &message, |current| {
                        // Eine unlesbare links.json nicht still durch eine
                        // frische Liste ersetzen — das schriebe den Verlust
                        // aller bisherigen Kanten aktiv fest. Die Lese-Seite
                        // (`links_at`) bleibt tolerant; nur das Zurückschreiben
                        // scheitert benannt.
                        let mut links: Vec<SessionLink> = match current {
                            Some(bytes) => match serde_json::from_slice(bytes) {
                                Ok(links) => links,
                                Err(_) => {
                                    corrupt = true;
                                    return None;
                                }
                            },
                            None => Vec::new(),
                        };

                        // Idempotent, und stärkere Herkunft gewinnt — dieselbe
                        // Regel wie in [`CommitIndex::link`], nur auf der Sicht
                        // *einer* Session.
                        match links.iter_mut().find(|link| link.commit == commit_hex) {
                            Some(existing) => {
                                if existing.evidence >= evidence {
                                    return None;
                                }
                                existing.evidence = evidence;
                            }
                            None => links.push(SessionLink {
                                commit: commit_hex.to_owned(),
                                evidence,
                            }),
                        }
                        links.sort_by(|a, b| a.commit.cmp(&b.commit));
                        Some(serde_json::to_vec(&links).expect("Kanten serialisieren immer"))
                    });
            match outcome {
                Ok(_) if corrupt => {
                    return Err(StoreError::CorruptLinks { reference });
                }
                Ok(_) => return Ok(()),
                Err(GitError::RefRaced { .. }) if attempts_left > 0 => {}
                Err(err) => return Err(StoreError::backend(err)),
            }
        }
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
        // Läufe schreiben verschiedene Inhalte an denselben Pfad. Der Retry
        // hält nur den Ref-Wechsel konsistent (kein Fork der Kette) — auf
        // **Inhaltsebene** bleibt `set_index` last-write-wins, denn der Merge
        // liegt beim Aufrufer außerhalb der Schleife (das Muster aus #4).
        // Vertretbar, weil der heiße Pfad (`link`) je Session schreibt und
        // `set_index` nur noch Import/Migration dient; wer hier nebenläufig
        // mergen will, braucht `update_blob_in_ref` wie `link`.
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

    /// Ersetzt die Nutzlast an **jedem** Ort, an dem sie liegt, durch einen
    /// elternlosen Tombstone — die Löschung überlebt keinen Ref-Rewalk mehr.
    ///
    /// Der Rumpf steht in [`forget_guarded`](Self::forget_guarded), das die
    /// Mehr-Ort-Logik und den Fehlerpfad ([`StoreError::ForgetIncomplete`])
    /// trägt; hier läuft es mit einem No-op-Guard.
    fn forget(&self, id: SessionId, reason: &str) -> Result<Forget> {
        self.forget_guarded(id, reason, |_| Ok(()))
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

        // Den Blob-Hash des Klartexts festhalten, bevor er vergessen wird.
        let payload_blob = fixture
            .git(&["rev-parse", &format!("{}:{SESSION_FILE}", session_ref(id))])
            .trim()
            .to_owned();

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

        // #14: Das Vergessen kappt die Historie — der Tombstone ist ein
        // **elternloser** Wurzel-Commit, kein aufgesetzter. Nur dieser eine
        // Commit hängt am Ref.
        assert_eq!(
            fixture
                .git(&["rev-list", "--count", &session_ref(id)])
                .trim(),
            "1"
        );
        assert_eq!(
            fixture
                .git(&["log", "-1", "--format=%P", &session_ref(id)])
                .trim(),
            "",
            "der Tombstone-Commit muss elternlos sein"
        );

        // Der Kern von #14 (Akzeptanzkriterium): Der Klartext-Blob ist über
        // **keinen** Ref mehr erreichbar — nach `gc` wäre er weg.
        let reachable = fixture.git(&["rev-list", "--objects", "--all"]);
        assert!(
            !reachable.contains(&payload_blob),
            "der Klartext-Blob {payload_blob} ist noch erreichbar:\n{reachable}"
        );
    }

    #[test]
    fn forget_deletes_a_tracking_ref_that_anchors_the_plaintext() {
        // #14 (B1): Nach einem Sync zeigt ein lokaler Tracking-Ref
        // (`refs/minds/remotes/<remote>/store/<hash>`) auf den Klartext-Commit.
        // Tilgt `forget` nur den maßgeblichen Store-Ref, hielte der Tracking-Ref
        // den Klartext-Blob erreichbar und gc-immun — die Löschung wäre lokal
        // unvollständig, während `rev-list` über die Session-Refs sauber aussieht.
        // `forget` löscht den Tracking-Ref deshalb (#102): De-ankern und zugleich
        // `minds sync` den Ref wieder anbieten lassen, damit der Tombstone die
        // Forge per gezieltem Force-Push erreicht.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();
        let payload_blob = fixture
            .git(&["rev-parse", &format!("{}:{SESSION_FILE}", session_ref(id))])
            .trim()
            .to_owned();
        let payload_commit = fixture
            .git(&["rev-parse", &session_ref(id)])
            .trim()
            .to_owned();

        // Ein früherer Sync: der Tracking-Ref verankert den Klartext-Commit.
        let tracking = format!("{TRACKING_REF_PREFIX}origin/store/{}", hex_of(id));
        fixture.git(&["update-ref", &tracking, &payload_commit]);
        let before = fixture.git(&["rev-list", "--objects", "--all"]);
        assert!(before.contains(&payload_blob), "Testaufbau: Blob nicht da");

        store.forget(id, "DSGVO").unwrap();

        // Der Klartext ist über keinen Ref mehr erreichbar …
        let after = fixture.git(&["rev-list", "--objects", "--all"]);
        assert!(
            !after.contains(&payload_blob),
            "Klartext-Blob über den Tracking-Ref noch erreichbar:\n{after}"
        );
        // … und der Tracking-Ref ist fort: `minds sync` sieht den Store-Ref als
        // ungetrackt, erkennt den Tombstone an der Spitze und überträgt die
        // Löschung per Force-Push zur Forge (#102).
        let refs = fixture.git(&["for-each-ref", "refs/minds/remotes"]);
        assert!(
            !refs.contains(&tracking),
            "Tracking-Ref nicht gelöscht:\n{refs}"
        );
    }

    #[test]
    fn a_tombstone_is_only_recognized_at_session_exclusive_refs() {
        // #102: Auf dieser Prüfung fußt die Force-Weiche in `minds sync` — sie
        // muss den Tombstone am Session-Ref erkennen und für alles andere
        // fail-closed `None` liefern, sonst wäre der Force-Push nicht auf die
        // Übertragung einer Löschung begrenzt.
        let (fixture, store) = fresh_store();
        let session = redacted("streng geheim");
        let id = store.put(&session).unwrap().id();
        let reference = session_ref(id);
        // Auch der browsbare Branch — sein Baum trägt `session.json` **und**
        // `session.md`, das prüft die Datei-Weiche gegen einen echten Baum.
        let bytes = SessionBytes::of(&session).unwrap();
        let markdown = minds_core::session_markdown(bytes.id(), session.session());
        store.put_session_branch_bytes(&bytes, &markdown).unwrap();
        let branch = session_branch_ref(id);

        let repo = Repo::open(fixture.path()).unwrap();
        let plain = repo.commit_at(&reference).unwrap().unwrap();
        assert_eq!(tombstone_at(&repo, &reference, plain), None, "Klartext");
        let plain_branch = repo.commit_at(&branch).unwrap().unwrap();
        assert_eq!(
            tombstone_at(&repo, &branch, plain_branch),
            None,
            "Klartext-Branch"
        );

        store.forget(id, "DSGVO-Antrag #42").unwrap();
        let repo = Repo::open(fixture.path()).unwrap();
        let tomb = repo.commit_at(&reference).unwrap().unwrap();
        assert_eq!(
            tombstone_at(&repo, &reference, tomb).as_deref(),
            Some("DSGVO-Antrag #42"),
            "Tombstone am Store-Ref"
        );
        let tomb_branch = repo.commit_at(&branch).unwrap().unwrap();
        assert_eq!(
            tombstone_at(&repo, &branch, tomb_branch).as_deref(),
            Some("DSGVO-Antrag #42"),
            "Tombstone am Session-Branch"
        );

        // Der geteilte Kontext-Ref fällt durch — selbst wenn er auf denselben
        // Commit zeigte, gehörte sein Baum nicht einer Session allein.
        assert_eq!(tombstone_at(&repo, DEFAULT_CONTEXT_REF, tomb), None);
        // Ein Ref außerhalb von refs/minds/ sowieso.
        assert_eq!(tombstone_at(&repo, "refs/heads/main", tomb), None);
    }

    #[test]
    fn a_second_forget_heals_a_reappeared_tracking_ref() {
        // #14-Blocker: Schlug das Umsetzen des Tracking-Refs beim ersten `forget`
        // fehl — oder legte ein späterer Sync ihn erneut auf den Klartext —, ist
        // der Store-Ref längst ein Tombstone, der Tracking-Ref hinge aber wieder
        // am Klartext. Ein zweiter `forget` muss das heilen, obwohl `payload_at`
        // am Store-Ref jetzt false ist (der Schritt hängt nicht am Payload).
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();
        let payload_blob = fixture
            .git(&["rev-parse", &format!("{}:{SESSION_FILE}", session_ref(id))])
            .trim()
            .to_owned();
        let payload_commit = fixture
            .git(&["rev-parse", &session_ref(id)])
            .trim()
            .to_owned();
        let tracking = format!("{TRACKING_REF_PREFIX}origin/store/{}", hex_of(id));

        // Erste Tilgung — der Store-Ref wird zum Tombstone.
        store.forget(id, "DSGVO").unwrap();
        // Der Tracking-Ref taucht (erneut) am Klartext-Commit auf.
        fixture.git(&["update-ref", &tracking, &payload_commit]);
        assert!(
            fixture
                .git(&["rev-list", "--objects", "--all"])
                .contains(&payload_blob),
            "Testaufbau: Klartext wieder erreichbar"
        );

        // Zweite Tilgung heilt, obwohl der Store-Ref schon ein Tombstone ist.
        store.forget(id, "DSGVO").unwrap();
        assert!(
            !fixture
                .git(&["rev-list", "--objects", "--all"])
                .contains(&payload_blob),
            "zweiter forget heilt den wieder-aufgetauchten Tracking-Ref nicht"
        );
    }

    #[test]
    fn forget_unanchors_a_tracking_ref_of_a_remote_with_a_slash() {
        // #14-Major: Der Remote-Name darf Schrägstriche enthalten. Ein
        // `refs/minds/remotes/team/origin/store/<hash>` muss trotzdem getroffen
        // werden, sonst überlebte der Klartext-Anker.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();
        let payload_blob = fixture
            .git(&["rev-parse", &format!("{}:{SESSION_FILE}", session_ref(id))])
            .trim()
            .to_owned();
        let payload_commit = fixture
            .git(&["rev-parse", &session_ref(id)])
            .trim()
            .to_owned();
        let tracking = format!("{TRACKING_REF_PREFIX}team/origin/store/{}", hex_of(id));
        fixture.git(&["update-ref", &tracking, &payload_commit]);

        store.forget(id, "DSGVO").unwrap();

        assert!(
            !fixture
                .git(&["rev-list", "--objects", "--all"])
                .contains(&payload_blob),
            "Tracking-Ref eines Remote mit / überlebt am Klartext"
        );
    }

    #[test]
    fn forget_unanchors_every_remote_tracking_ref_of_a_place() {
        // Ankern zwei Remotes denselben Klartext-Commit, muss `forget` **beide**
        // Tracking-Refs de-ankern — sonst hielte der übersehene den Klartext.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();
        let payload_blob = fixture
            .git(&["rev-parse", &format!("{}:{SESSION_FILE}", session_ref(id))])
            .trim()
            .to_owned();
        let payload_commit = fixture
            .git(&["rev-parse", &session_ref(id)])
            .trim()
            .to_owned();
        let t1 = format!("{TRACKING_REF_PREFIX}origin/store/{}", hex_of(id));
        let t2 = format!("{TRACKING_REF_PREFIX}team/mirror/store/{}", hex_of(id));
        fixture.git(&["update-ref", &t1, &payload_commit]);
        fixture.git(&["update-ref", &t2, &payload_commit]);

        store.forget(id, "DSGVO").unwrap();

        assert!(
            !fixture
                .git(&["rev-list", "--objects", "--all"])
                .contains(&payload_blob),
            "einer der beiden Tracking-Refs ankert den Klartext noch"
        );
    }

    #[test]
    fn forget_leaves_a_fast_forward_context_tracking_ref_alone() {
        // #14-Major: Der geteilte Kontext-Tracking-Ref darf NICHT umgesetzt
        // werden, wenn er bloß fast-forward hinter dem Kontext-HEAD zurückliegt
        // (ein Push anderer Sessions steht aus). Nur ein Orphan-Reset, der die
        // Kette kappt, de-ankert — sonst verschlänge `forget` einen legitimen,
        // noch ausstehenden Kontext-Push.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("streng geheim")).unwrap().id();

        // Den geteilten Kontext-Ref über den Index fast-forward fortschreiben.
        let mut index = store.index().unwrap();
        index.link("aaaa", id, Evidence::Inferred);
        store.set_index(&index).unwrap();
        let c1 = fixture
            .git(&["rev-parse", DEFAULT_CONTEXT_REF])
            .trim()
            .to_owned();
        // Der Kontext-Tracking-Ref liegt auf diesem früheren Stand.
        let tracking = format!("{TRACKING_REF_PREFIX}origin/context");
        fixture.git(&["update-ref", &tracking, &c1]);
        // Kontext fast-forward weiter (eine andere Kante).
        index.link("bbbb", id, Evidence::Inferred);
        store.set_index(&index).unwrap();
        let c2 = fixture
            .git(&["rev-parse", DEFAULT_CONTEXT_REF])
            .trim()
            .to_owned();
        assert_ne!(c1, c2, "Testaufbau: Kontext wanderte nicht");

        // forget der Session — sie liegt nur am Store-Ref, NICHT im Kontext-Baum.
        store.forget(id, "DSGVO").unwrap();

        // Der Kontext-Tracking-Ref blieb auf c1 (fast-forward, nicht umgesetzt):
        // `minds sync` kann den ausstehenden Kontext-Push regulär nachholen.
        let now = fixture.git(&["rev-parse", &tracking]).trim().to_owned();
        assert_eq!(
            now, c1,
            "fast-forward-Kontext-Tracking-Ref fälschlich umgesetzt"
        );
    }

    #[test]
    fn forget_keeps_the_side_files_of_the_store_ref() {
        // Der elternlose Reset des Store-Refs nimmt den aktuellen Baum als Basis,
        // damit Nebendateien (die Kanten in `links.json`) erhalten bleiben — nur
        // `session.json` wird zum Tombstone. Ein `write_tree(None, …)` wie beim
        // Branch verlöre sie.
        let (fixture, store) = fresh_store();
        let session = redacted("streng geheim");
        let id = store.put(&session).unwrap().id();
        let commit = "a".repeat(40);
        store.link(id, &commit, Evidence::Inferred).unwrap();
        // Vorbedingung: die Kante liegt am Store-Ref.
        let before = fixture.git(&["ls-tree", "-r", "--name-only", &session_ref(id)]);
        assert!(before.contains(SESSION_LINKS_FILE), "Testaufbau:\n{before}");

        store.forget(id, "DSGVO").unwrap();

        // Nach dem forget trägt der Store-Ref den Tombstone, aber `links.json`
        // steht weiter im aktuellen Baum.
        let after = fixture.git(&["ls-tree", "-r", "--name-only", &session_ref(id)]);
        assert!(
            after.contains(SESSION_LINKS_FILE),
            "links.json ging beim forget verloren:\n{after}"
        );
        let links = fixture.git(&[
            "cat-file",
            "blob",
            &format!("{}:{SESSION_LINKS_FILE}", session_ref(id)),
        ]);
        assert!(links.contains(&commit), "Kante verloren:\n{links}");
    }

    #[test]
    fn a_put_after_forget_does_not_resurrect_the_store_ref() {
        // #6 am echten Git-Backend: Nach `forget` prallt ein erneuter `put` am
        // Tombstone ab. Der Blob bleibt der Tombstone, kein neuer Commit — und
        // `get` meldet weiter Forgotten.
        let (fixture, store) = fresh_store();
        let session = redacted("streng geheim");
        let id = store.put(&session).unwrap().id();
        store.forget(id, "DSGVO").unwrap();

        let again = store.put(&session).unwrap();
        assert_eq!(again, Put::Forgotten(id));

        let revision = format!("{}:{SESSION_FILE}", session_ref(id));
        let blob = fixture.git(&["cat-file", "blob", &revision]);
        assert!(!blob.contains("streng geheim"), "reanimiert: {blob}");
        assert!(blob.contains("minds_tombstone"));
        // Der abgeprallte `put` schreibt nichts — es bleibt beim einen
        // elternlosen Tombstone-Commit aus dem `forget` (#14).
        assert_eq!(
            fixture
                .git(&["rev-list", "--count", &session_ref(id)])
                .trim(),
            "1"
        );
        assert!(matches!(store.get(id), Err(StoreError::Forgotten { .. })));
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
    fn concurrent_links_lose_no_edge() {
        // #4: Zwei Schreiber, dieselbe Session, verschiedene Commits. Vor dem
        // Fix mergte `link` außerhalb der CAS-Schleife — wer das Rennen verlor,
        // schrieb im Retry seine veralteten Bytes über die Kante des Gewinners,
        // und `why`/`show` fanden die Session über diesen Commit nicht mehr.
        let (fixture, store) = fresh_store();
        let id = store.put(&redacted("Wettlauf")).unwrap().id();
        let path = fixture.path().to_path_buf();

        let writers: Vec<_> = (0..2)
            .map(|writer| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = GitStore::new(Repo::open(&path).unwrap(), DEFAULT_CONTEXT_REF);
                    for i in 0..3 {
                        store
                            .link(id, &format!("c{writer}{i}"), Evidence::Inferred)
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let links = store.links_at(&session_ref(id)).unwrap();
        let log = fixture.git(&["log", "--format=%s", &session_ref(id)]);
        for commit in ["c00", "c01", "c02", "c10", "c11", "c12"] {
            assert!(
                links.iter().any(|link| link.commit == commit),
                "Kante {commit} fehlt\nlinks: {links:?}\nlog:\n{log}"
            );
        }
    }

    #[test]
    fn a_corrupt_links_file_is_not_clobbered() {
        // Aus dem Review zu #4: Eine unlesbare links.json darf beim Schreiben
        // nicht still durch eine frische Liste ersetzt werden — das nähme alle
        // bisherigen Kanten mit. Lesen bleibt tolerant, Schreiben scheitert
        // benannt.
        let (_fixture, store) = fresh_store();
        let id = store.put(&redacted("kaputte Kanten")).unwrap().id();
        store.link(id, "cafe", Evidence::Inferred).unwrap();

        store
            .repo
            .update_blob_in_ref(&session_ref(id), SESSION_LINKS_FILE, "kaputt", |_| {
                Some(b"{nicht json".to_vec())
            })
            .unwrap();

        let err = store.link(id, "beef", Evidence::Inferred).unwrap_err();
        assert!(matches!(err, StoreError::CorruptLinks { .. }), "{err:?}");
        assert_eq!(
            store
                .repo
                .read_blob_at(&session_ref(id), SESSION_LINKS_FILE)
                .unwrap()
                .unwrap(),
            b"{nicht json",
            "der kaputte Stand darf nicht überschrieben werden"
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
