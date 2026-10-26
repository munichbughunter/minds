//! In-Memory-Referenzimplementierung aller vier Traits. Sie dient den Tests
//! als Gerüst und späteren Adaptern (Schritt 3 und 6) als Vorlage für das
//! Vertragsverhalten. Sie ist öffentlich, aber nicht Teil der dokumentierten
//! API, und interpretiert nichts: was ein Eintrag bedeutet, legt der Test fest.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::CommitId;
use crate::pre::Decision;
use crate::push::PushContext;
use crate::receipt::{PublicKeyEntry, ReceiptKey};
use crate::traits::{
    CommitInfo, CommitRange, HostError, HostEvents, ObjectAccess, ObjectError, PutResult,
    ReceiptStore, SealBlob, SealError, SealExpectation, SealInspector, SealSummary, StoreError,
};

/// Vorgegebenes Ergebnis von `SealInspector::inspect` für ein Seal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SealOutcome {
    /// `Ok(summary)` unverändert.
    Summary(SealSummary),
    /// `Err(SealError::Malformed)`.
    Malformed,
    /// `Err(SealError::Internal)`.
    Internal,
}

/// Eingespeister Fehler; wirkt auf alle Aufrufe der jeweiligen Methode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fault {
    /// Kein Fehler.
    #[default]
    None,
    /// `ObjectAccess::commit` scheitert.
    CommitErr,
    /// `ObjectAccess::new_commits` scheitert.
    NewCommitsErr,
    /// `ObjectAccess::changed_paths` scheitert.
    ChangedPathsErr,
    /// `ObjectAccess::seal_for_commit` scheitert.
    SealLookupErr,
    /// `SealInspector::inspect` meldet für jedes Seal einen internen Fehler.
    SealInternal,
    /// `ReceiptStore::put` scheitert.
    PutErr,
    /// `ReceiptStore::register_public_key` scheitert.
    RegisterErr,
    /// `ObjectAccess::changed_paths` panict.
    PanicInChangedPaths,
}

/// In-Memory-Host, Objektsicht, Seal-Inspektor und Quittungsablage in einem.
#[doc(hidden)]
#[derive(Debug)]
pub struct MemoryAdapter {
    /// Push-Kontext, den `HostEvents::push_context` liefert.
    pub ctx: PushContext,
    /// Eingespeister Fehler.
    pub fault: Fault,
    /// Alle über `HostEvents::apply` empfangenen Entscheidungen.
    pub applied: Vec<Decision>,
    commits: BTreeMap<CommitId, CommitInfo>,
    changed: BTreeMap<CommitId, BTreeSet<String>>,
    seals: BTreeMap<CommitId, SealBlob>,
    outcomes: BTreeMap<String, SealOutcome>,
    receipts: BTreeMap<ReceiptKey, Vec<u8>>,
    keys: Vec<PublicKeyEntry>,
}

fn fault_err() -> ObjectError {
    ObjectError::Internal("fault".to_owned())
}

impl MemoryAdapter {
    /// Leerer Adapter für `ctx`.
    pub fn new(ctx: PushContext) -> Self {
        Self {
            ctx,
            fault: Fault::None,
            applied: Vec::new(),
            commits: BTreeMap::new(),
            changed: BTreeMap::new(),
            seals: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            receipts: BTreeMap::new(),
            keys: Vec::new(),
        }
    }

    /// Legt einen Commit mit Eltern und geänderten Pfaden an (ersetzt einen
    /// vorhandenen).
    pub fn add_commit(
        &mut self,
        id: CommitId,
        parents: Vec<CommitId>,
        changed: BTreeSet<String>,
    ) -> &mut Self {
        self.changed.insert(id.clone(), changed);
        self.commits.insert(id.clone(), CommitInfo { id, parents });
        self
    }

    /// Ordnet `commit` ein Seal unter `ref_name` zu und legt fest, was der
    /// Inspektor dafür liefert. Ein vorhandenes Seal des Commits wird ersetzt.
    pub fn add_seal(
        &mut self,
        commit: CommitId,
        ref_name: &str,
        bytes: Vec<u8>,
        outcome: SealOutcome,
    ) -> &mut Self {
        if let Some(old) = self.seals.remove(&commit) {
            self.outcomes.remove(&old.ref_name);
        }
        self.outcomes.insert(ref_name.to_owned(), outcome);
        self.seals.insert(
            commit,
            SealBlob {
                ref_name: ref_name.to_owned(),
                bytes,
            },
        );
        self
    }

    /// Entfernt Seal und Outcome; unbekannter Commit ist kein Fehler.
    pub fn remove_seal(&mut self, commit: &CommitId) -> &mut Self {
        if let Some(old) = self.seals.remove(commit) {
            self.outcomes.remove(&old.ref_name);
        }
        self
    }

    /// Nur für Konflikt-Tests: legt einen Eintrag direkt ab.
    pub fn seed_receipt(&mut self, key: ReceiptKey, bytes: Vec<u8>) -> &mut Self {
        self.receipts.insert(key, bytes);
        self
    }

    /// Nur für Tests (Konflikte, Widerrufe, widersprüchliche Einträge): legt
    /// einen Schlüsseleintrag direkt ab.
    pub fn seed_public_key(&mut self, entry: PublicKeyEntry) -> &mut Self {
        self.keys.push(entry);
        self
    }

    /// Alle Quittungsbytes zu `commit`, aufsteigend nach `ReceiptKey::id`.
    pub fn receipts_for(&self, commit: &CommitId) -> Vec<Vec<u8>> {
        self.receipts
            .iter()
            .filter(|(k, _)| k.commit == *commit)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// Anzahl aller abgelegten Quittungen.
    pub fn receipt_count(&self) -> usize {
        self.receipts.len()
    }

    /// Die Schlüsselliste.
    pub fn keys(&self) -> Vec<PublicKeyEntry> {
        self.keys.clone()
    }

    /// Alle von `x` über `parents` erreichbaren Commits einschließlich `x`.
    fn reachable(&self, x: &CommitId) -> BTreeSet<CommitId> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![x.clone()];
        while let Some(c) = stack.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(info) = self.commits.get(&c) {
                // Unbekannte Eltern werden ignoriert.
                stack.extend(
                    info.parents
                        .iter()
                        .filter(|p| self.commits.contains_key(*p))
                        .cloned(),
                );
            }
        }
        seen
    }
}

impl HostEvents for MemoryAdapter {
    fn push_context(&self) -> Result<PushContext, HostError> {
        Ok(self.ctx.clone())
    }

    fn apply(&mut self, decision: &Decision) {
        self.applied.push(decision.clone());
    }
}

impl ObjectAccess for MemoryAdapter {
    fn commit(&self, id: &CommitId) -> Result<CommitInfo, ObjectError> {
        if self.fault == Fault::CommitErr {
            return Err(fault_err());
        }
        self.commits
            .get(id)
            .cloned()
            .ok_or_else(|| ObjectError::NotFound(id.as_str().to_owned()))
    }

    fn new_commits(
        &self,
        old: Option<&CommitId>,
        new: &CommitId,
        limit: u32,
    ) -> Result<CommitRange, ObjectError> {
        if self.fault == Fault::NewCommitsErr {
            return Err(fault_err());
        }
        let mut set = self.reachable(new);
        if let Some(old) = old {
            for c in self.reachable(old) {
                set.remove(&c);
            }
        }
        // Kahn-Algorithmus, Kanten Elternteil → Kind, kleinste Id zuerst.
        let mut indegree: BTreeMap<CommitId, usize> = BTreeMap::new();
        let mut children: BTreeMap<CommitId, Vec<CommitId>> = BTreeMap::new();
        for c in &set {
            let parents: BTreeSet<&CommitId> = self
                .commits
                .get(c)
                .map(|i| i.parents.iter().filter(|p| set.contains(*p)).collect())
                .unwrap_or_default();
            indegree.insert(c.clone(), parents.len());
            for p in parents {
                children.entry(p.clone()).or_default().push(c.clone());
            }
        }
        let mut ready: BTreeSet<CommitId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(c, _)| c.clone())
            .collect();
        let mut order = Vec::with_capacity(set.len());
        while let Some(c) = ready.pop_first() {
            if let Some(kids) = children.get(&c) {
                for k in kids {
                    if let Some(d) = indegree.get_mut(k) {
                        *d -= 1;
                        if *d == 0 {
                            ready.insert(k.clone());
                        }
                    }
                }
            }
            order.push(c);
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let truncated = set.len() > limit;
        order.truncate(limit);
        Ok(CommitRange {
            commits: order,
            truncated,
        })
    }

    fn changed_paths(&self, id: &CommitId) -> Result<BTreeSet<String>, ObjectError> {
        match self.fault {
            Fault::ChangedPathsErr => return Err(fault_err()),
            Fault::PanicInChangedPaths => panic!("fault"),
            _ => {}
        }
        self.changed
            .get(id)
            .cloned()
            .ok_or_else(|| ObjectError::NotFound(id.as_str().to_owned()))
    }

    fn seal_for_commit(&self, id: &CommitId) -> Result<Option<SealBlob>, ObjectError> {
        if self.fault == Fault::SealLookupErr {
            return Err(fault_err());
        }
        Ok(self.seals.get(id).cloned())
    }
}

impl SealInspector for MemoryAdapter {
    fn inspect(
        &self,
        blob: &SealBlob,
        _expected: &SealExpectation,
    ) -> Result<SealSummary, SealError> {
        if self.fault == Fault::SealInternal {
            return Err(SealError::Internal("fault".to_owned()));
        }
        match self.outcomes.get(&blob.ref_name) {
            Some(SealOutcome::Summary(s)) => Ok(s.clone()),
            Some(SealOutcome::Malformed) => Err(SealError::Malformed("fault".to_owned())),
            Some(SealOutcome::Internal) => Err(SealError::Internal("fault".to_owned())),
            None => Err(SealError::Malformed("unbekanntes Seal".to_owned())),
        }
    }
}

impl ReceiptStore for MemoryAdapter {
    fn put(&mut self, key: &ReceiptKey, bytes: &[u8]) -> Result<PutResult, StoreError> {
        if self.fault == Fault::PutErr {
            return Err(StoreError::Internal("fault".to_owned()));
        }
        match self.receipts.get(key) {
            Some(existing) if existing.as_slice() == bytes => Ok(PutResult::AlreadyExists),
            Some(_) => Err(StoreError::Conflict(key.id.as_str().to_owned())),
            None => {
                self.receipts.insert(key.clone(), bytes.to_vec());
                Ok(PutResult::Stored)
            }
        }
    }

    fn get(&self, key: &ReceiptKey) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.receipts.get(key).cloned())
    }

    fn list(&self, commit: &CommitId) -> Result<Vec<ReceiptKey>, StoreError> {
        Ok(self
            .receipts
            .keys()
            .filter(|k| k.commit == *commit)
            .cloned()
            .collect())
    }

    fn public_keys(&self) -> Result<Vec<PublicKeyEntry>, StoreError> {
        Ok(self.keys.clone())
    }

    fn register_public_key(&mut self, entry: &PublicKeyEntry) -> Result<(), StoreError> {
        if self.fault == Fault::RegisterErr {
            return Err(StoreError::Internal("fault".to_owned()));
        }
        // Spec S1 v1.4 §11: Bei mehreren Einträgen desselben Schlüssels genügt einer
        // mit gleichem `valid_from`; nur wenn keiner passt, ist es ein Konflikt.
        let same_key: Vec<&PublicKeyEntry> =
            self.keys.iter().filter(|k| k.key == entry.key).collect();
        if same_key.is_empty() {
            // `valid_until` wird nie gesetzt: neu angelegte Einträge tragen
            // genau den übergebenen Wert.
            self.keys.push(entry.clone());
            Ok(())
        } else if same_key.iter().any(|k| k.valid_from == entry.valid_from) {
            Ok(())
        } else {
            Err(StoreError::Conflict(entry.key.as_str().to_owned()))
        }
    }
}
