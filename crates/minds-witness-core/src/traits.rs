//! Vertrag zwischen Kern und Host: vier Traits plus ihre Hilfs- und
//! Fehlertypen. Der Kern ruft `ObjectAccess`, `SealInspector` und
//! `ReceiptStore`; `HostEvents` ruft nur der Adapter. Das Modul enthält keine
//! Implementierung und kennt weder Git noch das Seal-Format.

use std::collections::BTreeSet;

use crate::ids::{CommitId, HashStr};
use crate::pre::Decision;
use crate::push::PushContext;
use crate::receipt::{PublicKeyEntry, ReceiptKey};

/// Lieferant des Push-Kontexts und Empfänger der Entscheidung. Nur vom
/// Adapter aufgerufen.
pub trait HostEvents {
    /// Der Kontext des aktuellen Pushes.
    fn push_context(&self) -> Result<PushContext, HostError>;
    /// Teilt dem Host die Entscheidung mit.
    fn apply(&mut self, decision: &Decision);
}

/// Fehler beim Bereitstellen des Push-Kontexts.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// Der Host konnte keinen gültigen Kontext liefern.
    #[error("Push-Kontext ungültig: {0}")]
    Invalid(String),
}

/// Lesesicht auf Objekte im Vorzustand des Pushes.
///
/// Spec S1 v1.5 §5 (V4): Die Sicht muss kein atomarer Schnappschuss sein, darf
/// sich aber innerhalb eines `pre_receive`-Aufrufs nicht ändern. Push-weite
/// Budgets oder eine Deadline des Adapters wirken nur als
/// `ObjectError::Internal`.
pub trait ObjectAccess {
    /// Commit-Metadaten. Fehlt der Commit: `ObjectError::NotFound`.
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError>;
    /// Neue Commits `M` (Abschnitt 2). `old == None` steht für die Null-Id.
    /// `truncated == false`: die vollständige Menge `M`, topologisch, ältester zuerst.
    /// `truncated == true` genau dann, wenn `|M| > limit`; zugesichert ist dann nur
    /// `commits.len() == limit` und jedes Element ∈ `M` ohne Doppelte, nicht welche
    /// und nicht in welcher Reihenfolge (Spec S1 v1.5, V7). Der Kern nutzt dann nur
    /// die Länge.
    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError>;
    /// Geänderte Pfade (Abschnitt 2): für Nicht-Merges gegenüber dem ersten Elternteil, für Merges die mergeeigenen Änderungen.
    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError>;
    /// Das dem Commit zugeordnete Seal, sofern vorhanden. Mehrere einzelne Seals: das mit
    /// dem lexikografisch kleinsten `ref_name`. Zusätzlich zulässig (Spec S1 v1.5, V3):
    /// eine Kandidatenmenge als **ein** Blob; `ref_name` ist dann ein Präfix mit `/` am
    /// Ende und kein gültiger Ref-Name, der Inspektor vereinigt über die akzeptierten
    /// Elemente.
    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError>;
}

/// Metadaten eines Commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    /// Id des Commits.
    pub id: CommitId,
    /// Eltern in Commit-Reihenfolge; zwei oder mehr bedeuten Merge.
    pub parents: Vec<CommitId>,
}

/// Antwort von `ObjectAccess::new_commits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRange {
    /// Neue Commits. Bei `truncated == false` alle, ältester zuerst; bei
    /// `truncated == true` genau `limit` verschiedene Elemente der Menge in beliebiger Auswahl
    /// und Reihenfolge (Spec S1 v1.5, V7).
    pub commits: Vec<CommitId>,
    /// `true` genau dann, wenn mehr als `limit` neue Commits existieren.
    pub truncated: bool,
}

/// Rohes Seal, wie es der Host gefunden hat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealBlob {
    /// Ref, unter dem das Seal liegt, oder bei einer Kandidatenmenge deren Präfix mit
    /// `/` am Ende (Spec S1 v1.5, V3). Welcher Namensraum das ist, legt der Adapter
    /// fest (im Bestand `refs/minds/evidence/<seal_id>`); der Kern wertet den Namen
    /// nur als Kennung aus und übernimmt ihn unverändert in die Quittung.
    pub ref_name: String,
    /// Inhalt des Seal-Blobs; verlässt den Kern nie in Ereignissen.
    pub bytes: Vec<u8>,
}

/// Was der Kern vom Seal erwartet; der Inspektor darf es zur Prüfung nutzen, der Kern prüft `bound_commit` selbst.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealExpectation {
    /// Erwartete Repository-Kennung (`PushContext::repo`).
    pub repo: String,
    /// Commit, für den das Seal geprüft wird.
    pub commit: CommitId,
}

/// Fehler beim Objektzugriff.
#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    /// Objekt existiert nicht.
    #[error("Objekt nicht gefunden: {0}")]
    NotFound(String),
    /// Technischer Fehler beim Zugriff.
    #[error("Objektzugriff fehlgeschlagen: {0}")]
    Internal(String),
}

/// Deutet ein Seal, ohne dass der Kern sein Format kennt.
pub trait SealInspector {
    /// Fasst `blob` zusammen; Vertrag siehe Abschnitt 5 der Spec. Das Ergebnis hängt
    /// nur von `blob`, `expected` und der für einen `pre_receive`-Aufruf festen Sicht
    /// des Adapters ab, nie vom bewerteten Ref, von der Policy oder anderem
    /// Push-Zustand des Kerns; Budgets und Deadline wirken nur als
    /// `SealError::Internal` (Spec S1 v1.5, V4).
    fn inspect(
        &self,
        blob: &SealBlob,
        expected: &SealExpectation,
    ) -> Result<SealSummary, SealError>;
}

/// Signaturzustand eines Seals laut Inspektor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SealSignature {
    /// Signatur der Evidence (Seal oder Binding-Attestation, Spec S1 v1.5, V1)
    /// verifiziert, Signierer als lokaler Witness-Schlüssel anerkannt.
    Valid,
    /// Signatur falsch oder Schlüssel nicht anerkannt.
    Invalid,
    /// Keine Signatur.
    Unsigned,
}

/// Zusammenfassung eines Seals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealSummary {
    /// Hash, der die Quittung an die geprüfte Evidence bindet und den ein Verifizierer
    /// aus dem Repository nachrechnet (Adapter-Sache); für Bindungsmengen nach S2 §8.4
    /// (Spec S1 v1.5, V5).
    pub hash: HashStr,
    /// Signaturzustand.
    pub signature: SealSignature,
    /// Commit, an den die Evidence gebunden ist. Bei `signature == Valid` ausschließlich
    /// aus signiertem Inhalt (Seal oder Binding-Attestation); bei `Invalid`/`Unsigned`
    /// darf der Inspektor `expected.commit` übernehmen (Spec S1 v1.5, V1). Der Kern prüft
    /// die Bindung vor der Signatur.
    pub bound_commit: CommitId,
    /// Repository-Kennung der Bindung, falls das Format eine trägt; `None` heißt: keine
    /// Repo-Bindung vorhanden. Herkunft wie `bound_commit` (Spec S1 v1.5, V1); bei
    /// `Valid` ohne Repo-Kennung im Format bleibt es `None`, nie `expected.repo`.
    pub bound_repo: Option<String>,
    /// Pfade, deren committeten Zustand die geprüfte Evidence inhaltsgebunden belegt
    /// (Spec S1 v1.5, V2; Abschnitt 2, Form "Pfad"); nie ein Pfad der Form
    /// `"\0nonutf8:…"` (V6).
    pub touched_paths: BTreeSet<String>,
}

/// Fehler des Seal-Inspektors.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// Bytes sind kein Seal. Der Kern wertet das als Seal mit Status `Invalid`.
    #[error("Seal nicht lesbar: {0}")]
    Malformed(String),
    /// Technischer Fehler des Inspektors. Der Kern wertet das als internen Fehler.
    #[error("Seal-Inspektion fehlgeschlagen: {0}")]
    Internal(String),
}

/// Ablage der Quittungen und der Schlüsselliste.
pub trait ReceiptStore {
    /// Legt `bytes` unter `key` ab. Existiert unter genau diesem `key` bereits ein Eintrag mit byteidentischem Inhalt: `Ok(AlreadyExists)`.
    /// Existiert ein Eintrag mit abweichendem Inhalt: `Err(StoreError::Conflict)`, und der vorhandene bleibt unverändert.
    fn put(&mut self, key: &ReceiptKey, bytes: &[u8]) -> Result<PutResult, StoreError>;
    /// Originalbytes der Quittung unter `key`, sofern vorhanden.
    fn get(&self, key: &ReceiptKey) -> Result<Option<Vec<u8>>, StoreError>;
    /// Alle Quittungen zu einem Commit, aufsteigend nach `id` sortiert.
    fn list(&self, commit: &CommitId) -> Result<Vec<ReceiptKey>, StoreError>;
    /// Die bekannte Schlüsselliste, einschließlich aller Widerrufe
    /// (`valid_until`). Die Post-Phase liest sie nach der Registrierung und
    /// signiert nur, wenn jeder Eintrag des Signierschlüssels den
    /// Serverzeitpunkt des Befunds zulässt; ein Fehler hier verhindert jede
    /// Signatur des Aufrufs.
    fn public_keys(&self) -> Result<Vec<PublicKeyEntry>, StoreError>;
    /// Ein Eintrag mit gleichem `key` und gleichem `valid_from` wird nicht doppelt angelegt und nicht verändert (`Ok`).
    /// Ein Eintrag mit gleichem `key`, aber anderem `valid_from`: `Err(StoreError::Conflict)`.
    /// `valid_until` wird von dieser Methode nie gesetzt, entfernt oder verändert: ein Eintrag, den die Schlüsselverwaltung (Schritt 3)
    /// mit `valid_until` zurückgezogen hat, bleibt durch eine erneute Registrierung zurückgezogen. Eine Registrierung kann einen
    /// Gültigkeitszeitraum also nie erweitern, nur einen neuen Schlüssel anlegen.
    fn register_public_key(&mut self, entry: &PublicKeyEntry) -> Result<(), StoreError>;
}

/// Ergebnis von `ReceiptStore::put`.
#[derive(Debug, PartialEq, Eq)]
pub enum PutResult {
    /// Neu abgelegt.
    Stored,
    /// Byteidentischer Eintrag existierte bereits.
    AlreadyExists,
}

/// Fehler des Stores.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Technischer Fehler.
    #[error("Store-Fehler: {0}")]
    Internal(String),
    /// Gleicher Schlüssel, anderer Inhalt: wird nie stillschweigend als Erfolg behandelt.
    #[error("Store-Konflikt: {0}")]
    Conflict(String),
}
