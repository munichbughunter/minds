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
pub trait ObjectAccess {
    /// Commit-Metadaten. Fehlt der Commit: `ObjectError::NotFound`.
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError>;
    /// Neue Commits (Abschnitt 2), höchstens `limit` Stück, ältester zuerst.
    /// `truncated == true`, wenn mehr als `limit` existieren; die Liste enthält dann
    /// die `limit` ältesten. `old == None` steht für die Null-Id.
    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError>;
    /// Geänderte Pfade (Abschnitt 2): für Nicht-Merges gegenüber dem ersten Elternteil, für Merges die mergeeigenen Änderungen.
    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError>;
    /// Das dem Commit zugeordnete Seal, sofern vorhanden. Mehrere Seals: das mit dem
    /// lexikografisch kleinsten `ref_name`.
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
    /// Neue Commits, ältester zuerst, höchstens `limit` Stück.
    pub commits: Vec<CommitId>,
    /// `true`, wenn mehr als `limit` neue Commits existieren.
    pub truncated: bool,
}

/// Rohes Seal, wie es der Host gefunden hat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealBlob {
    /// Ref, unter dem das Seal liegt. Welcher Namensraum das ist, legt der
    /// Adapter fest (im Bestand `refs/minds/evidence/<seal_id>`); der Kern
    /// wertet den Namen nur als Kennung aus.
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
    /// Fasst `blob` zusammen; Vertrag siehe Abschnitt 5 der Spec.
    fn inspect(
        &self,
        blob: &SealBlob,
        expected: &SealExpectation,
    ) -> Result<SealSummary, SealError>;
}

/// Signaturzustand eines Seals laut Inspektor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SealSignature {
    /// Signatur verifiziert, Schlüssel als lokaler Witness-Schlüssel anerkannt.
    Valid,
    /// Signatur falsch oder Schlüssel nicht anerkannt.
    Invalid,
    /// Keine Signatur.
    Unsigned,
}

/// Zusammenfassung eines Seals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealSummary {
    /// Hash des Seal-Inhalts, wie `minds verify` ihn berechnet (Adapter-Sache).
    pub hash: HashStr,
    /// Signaturzustand.
    pub signature: SealSignature,
    /// Commit, an den das Seal kryptografisch gebunden ist; aus dem signierten Inhalt des Seals gelesen, nie aus `expected` übernommen.
    pub bound_commit: CommitId,
    /// Repository-Kennung aus dem signierten Inhalt des Seals, falls das Seal-Format eine trägt; `None` heißt: keine Repo-Bindung vorhanden.
    pub bound_repo: Option<String>,
    /// Pfade, die die Evidence-Kette des Seals berührt (Abschnitt 2, Form "Pfad").
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
